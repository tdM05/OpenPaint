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

use openpaint_core::{Brush, Dab, Document, Page, PageRect, StrokeState};

/// Starting page size. A placeholder until New-document presets exist (§5a says
/// "300 DPI A4" and friends are presets computing pixel dimensions).
pub const PAGE_W: u32 = 2048;
pub const PAGE_H: u32 = 2048;

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
        /// Whether this stroke removes paint instead of adding it.
        ///
        /// Captured at stroke start like the colour, and for the same reason: switching tool
        /// mid-stroke would otherwise produce a stroke that is half paint and half hole.
        mode: PaintMode,
    },
    /// Stamp dabs `[start, start + len)` of the accompanying dab buffer.
    Dabs { start: usize, len: usize },
    /// Commit the stroke into the canvas.
    ///
    /// Carries nothing: the renderer knows exactly which tiles the stroke reached, so
    /// asking the editor to track a bounding rectangle for history's benefit was both
    /// less precise and a second place for the page-clipping rule to live.
    End,
}

/// What a stroke does to the layer it lands on.
///
/// Deliberately not a "brush type": the dab geometry, spacing, falloff and pressure response are
/// identical, and the only difference is how the accumulated coverage is composited -- paint
/// blends over, erase multiplies by its complement. Anything that changed the *shape* of a stroke
/// would belong in `openpaint_core::brush` instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Tool {
    #[default]
    Brush,
    Eraser,
}

impl Tool {
    /// Every tool, in the order the UI lists them.
    pub const ALL: [Self; 2] = [Self::Brush, Self::Eraser];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Brush => "Brush",
            Self::Eraser => "Eraser",
        }
    }

    #[must_use]
    pub fn erases(self) -> bool {
        matches!(self, Self::Eraser)
    }
}

/// Largest page dimension, in pixels.
///
/// **Not** a GPU limit any more. The canvas is a pool of tiles, so no allocation is
/// proportional to page size and `max_texture_dimension_2d` no longer applies —
/// the previous 8192 px and 16 Mpx ceilings both existed only because the canvas
/// was one texture, and both are gone.
///
/// What remains is a coordinate-precision limit. Page positions are `f32` in the
/// camera and in dab geometry, and `f32` represents every integer exactly only up
/// to 2^24. 65536 leaves a factor of 256 of headroom for intermediate sums, and is
/// past any real artwork: at 300 DPI it is a 5.5-metre page.
pub const MAX_PAGE_DIMENSION: u32 = 65_536;

/// Clamp a requested page size to what coordinates can represent.
///
/// Returns `None` when the result would not change the page, so the caller can say so
/// instead of performing a resize that does nothing.
#[must_use]
pub fn clamp_page_size(current: (u32, u32), requested: (u32, u32)) -> Option<(u32, u32)> {
    let w = requested.0.clamp(1, MAX_PAGE_DIMENSION);
    let h = requested.1.clamp(1, MAX_PAGE_DIMENSION);
    if (w, h) == current {
        return None;
    }
    Some((w, h))
}

/// How a stroke is combined with the layer it lands on.
///
/// An enum rather than two booleans because exactly one applies. "Erase" and "lock alpha" are
/// contradictory instructions — one removes coverage, the other forbids coverage from changing —
/// so a state carrying both would have no defined meaning, and the compiler should not let one be
/// constructed. See [`openpaint_core::Layer::locks_alpha`].
///
/// Each mode is the *same* dab rasterization with a different blend, which is why adding one costs
/// a `BlendState` and not a code path: see `stroke_layer.rs`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaintMode {
    /// Paint over what is there.
    Normal,
    /// Remove coverage.
    Erase,
    /// Paint only where coverage already exists, leaving alpha exactly as it was.
    LockAlpha,
}

pub struct Editor {
    document: Document,
    /// One brush per tool, indexed by `Tool as usize`.
    ///
    /// Separate settings because an eraser almost always wants a different size from the brush,
    /// and sharing one would make every size change fight the other tool. An array rather than
    /// named fields so a third tool is a variant and a default, not a new field everywhere.
    brushes: [Brush; Tool::ALL.len()],
    tool: Tool,
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
            document: Document::new(Page::new(PAGE_W, PAGE_H)),
            brushes: [Brush::default(), Brush::default()],
            tool: Tool::Brush,
            stroke: StrokeState::new(),
            dabs: Vec::new(),
            ops: Vec::new(),
            drawing: false,
        }
    }

    /// The rectangle of the page being edited.
    ///
    /// The page owns its geometry; layers have none of their own (`openpaint_core::page`).
    #[must_use]
    pub fn page_rect(&self) -> PageRect {
        self.document.active().rect()
    }

    /// The id of the layer being painted, which is what the renderer keys tiles by.
    #[must_use]
    pub fn active_layer_id(&self) -> u32 {
        self.document.active().active_layer().id()
    }

    /// The stack of the page being edited, bottom layer first.
    #[must_use]
    pub fn layers(&self) -> &[openpaint_core::Layer] {
        self.document.active().layers()
    }

    /// Index of the layer being painted.
    #[must_use]
    pub fn active_layer_index(&self) -> usize {
        self.document.active().active_index()
    }

    /// Replace the open document wholesale, for loading a file.
    ///
    /// Ends any stroke first: it belongs to a layer of the document being replaced.
    pub fn replace_document(&mut self, document: Document) {
        self.stroke_end();
        self.dabs.clear();
        self.ops.clear();
        self.document = document;
    }

    /// Mutable document access, for layer commands from the UI.
    pub fn document_mut(&mut self) -> &mut Document {
        &mut self.document
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
    /// Which tool strokes currently use.
    #[must_use]
    pub fn tool(&self) -> Tool {
        self.tool
    }

    /// Switch tool. Ends any stroke first, since a stroke's mode is fixed at its start.
    pub fn set_tool(&mut self, tool: Tool) {
        if self.tool != tool {
            self.stroke_end();
            self.tool = tool;
        }
    }

    /// The active tool's brush.
    #[must_use]
    pub fn brush(&self) -> &Brush {
        &self.brushes[self.tool as usize]
    }

    /// The brush a colour sample should land on, whatever tool is active.
    ///
    /// Separate from [`Editor::brush_mut`], which follows the active tool. An eyedropper used while
    /// the eraser is selected still means "this is what I want to paint with", and the eraser has no
    /// colour to set.
    pub fn paint_brush_mut(&mut self) -> &mut Brush {
        &mut self.brushes[Tool::Brush as usize]
    }

    pub fn brush_mut(&mut self) -> &mut Brush {
        &mut self.brushes[self.tool as usize]
    }

    /// Whether a stroke is currently in progress.
    #[must_use]
    pub fn is_drawing(&self) -> bool {
        self.drawing
    }

    /// How the next stroke would be applied, or `None` if it could not do anything.
    ///
    /// `None` is erasing on an alpha-locked layer. Alpha lock means alpha cannot change, and
    /// erasing is nothing but a change in alpha, so the combination has no possible effect. Refused
    /// rather than performed as an invisible no-op: a tool that silently does nothing reads as a
    /// broken app, and the caller can say why instead.
    #[must_use]
    pub fn paint_mode(&self) -> Option<PaintMode> {
        let locked = self
            .document
            .active()
            .layer(self.active_layer_index())
            .is_some_and(openpaint_core::Layer::locks_alpha);
        match (self.tool.erases(), locked) {
            (true, true) => None,
            (true, false) => Some(PaintMode::Erase),
            (false, true) => Some(PaintMode::LockAlpha),
            (false, false) => Some(PaintMode::Normal),
        }
    }

    /// Begin a stroke at a canvas-space position.
    ///
    /// Does nothing when [`Editor::paint_mode`] has no mode to offer. Checked here as well as by the
    /// caller so that neither layer depends on the other remembering.
    pub fn stroke_begin(&mut self, cx: f32, cy: f32, pressure: f32) {
        let Some(mode) = self.paint_mode() else {
            return;
        };
        // A fresh stroke resets accumulation, so its opacity ceiling is
        // independent of the previous stroke's.
        self.ops.push(StrokeOp::Begin {
            color_linear_premul: self.brush().color_linear_premul(),
            opacity: self.brush().opacity,
            mode,
        });
        self.drawing = true;
        let from = self.dabs.len();
        self.brushes[self.tool as usize].stroke_begin(
            &mut self.dabs,
            &mut self.stroke,
            cx,
            cy,
            pressure,
        );
        self.record_dabs(from);
    }

    /// Continue the current stroke to a new canvas-space position.
    pub fn stroke_to(&mut self, cx: f32, cy: f32, pressure: f32) {
        if !self.drawing {
            return;
        }
        let from = self.dabs.len();
        self.brushes[self.tool as usize].stroke_to(
            &mut self.dabs,
            &mut self.stroke,
            cx,
            cy,
            pressure,
        );
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
        if len == 0 {
            return;
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
        assert_eq!(e.layers().len(), 1);
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
                ..
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

    /// Ending a stroke must queue exactly one commit, and it carries no geometry: which
    /// tiles were written is the renderer's knowledge, and duplicating it here was a
    /// second place for the page-clipping rule to disagree with itself.
    #[test]
    fn end_queues_a_bare_commit() {
        let mut e = Editor::new();
        e.brush_mut().radius = 10.0;
        e.stroke_begin(200.0, 300.0, 1.0);
        e.stroke_to(400.0, 300.0, 1.0);
        e.stroke_end();

        let (ops, _) = e.pending_stroke();
        assert_eq!(ops.last(), Some(&StrokeOp::End));
        assert_eq!(
            ops.iter().filter(|o| matches!(o, StrokeOp::End)).count(),
            1,
            "exactly one commit per stroke"
        );
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

    #[test]
    fn a_normal_extend_is_not_clamped() {
        assert_eq!(
            clamp_page_size((2048, 2048), (2048, 2560)),
            Some((2048, 2560))
        );
    }

    /// Past the coordinate-precision ceiling, the request is clamped rather than refused.
    #[test]
    fn extending_past_the_limit_clamps_to_it() {
        assert_eq!(
            clamp_page_size((2048, 2048), (2048, MAX_PAGE_DIMENSION + 1)),
            Some((2048, MAX_PAGE_DIMENSION))
        );
    }

    #[test]
    fn width_and_height_clamp_independently() {
        assert_eq!(
            clamp_page_size((MAX_PAGE_DIMENSION, 1000), (MAX_PAGE_DIMENSION + 9, 2000)),
            Some((MAX_PAGE_DIMENSION, 2000))
        );
    }

    #[test]
    fn a_zero_request_is_clamped_to_something_drawable() {
        assert_eq!(clamp_page_size((100, 100), (0, 0)), Some((1, 1)));
    }

    /// Already at the ceiling: there is nothing to do, and the caller needs to know
    /// rather than resize to the same size and report success.
    #[test]
    fn no_change_reports_none() {
        assert_eq!(
            clamp_page_size(
                (2048, MAX_PAGE_DIMENSION),
                (2048, MAX_PAGE_DIMENSION + 1024)
            ),
            None
        );
        assert_eq!(clamp_page_size((2048, 2048), (2048, 2048)), None);
    }

    /// The sizes the old single-texture ceilings refused must now be allowed: that they
    /// were refused at all was the cost of the shortcut, and removing it is the point of
    /// the tiled canvas.
    #[test]
    fn sizes_the_single_texture_ceiling_refused_are_now_allowed() {
        // Past the old 8192 px dimension limit.
        assert_eq!(
            clamp_page_size((800, 8192), (800, 20_000)),
            Some((800, 20_000)),
            "a real webtoon strip must be allowed"
        );
        // Past the old 16 Mpx budget: a fully-inked A4 at 600 DPI.
        assert_eq!(
            clamp_page_size((100, 100), (4960, 7016)),
            Some((4960, 7016))
        );
        // And well past 8192 in both directions at once.
        assert_eq!(
            clamp_page_size((100, 100), (10_000, 10_000)),
            Some((10_000, 10_000))
        );
    }
}
