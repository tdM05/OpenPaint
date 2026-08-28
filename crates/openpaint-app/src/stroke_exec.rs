//! Executes the editor's stroke command stream on the GPU.
//!
//! Split out of [`crate::renderer::Renderer`] because this is where the ordering rules live
//! (DECISIONS §11a.2) and it was the one part of the paint path that could not be tested:
//! it needed a `Renderer`, and a `Renderer` needs a window and a surface. Everything here
//! works on a device, a queue, and the two tile pools, so a headless test can drive a real
//! `Editor` through it and read the pixels back.
//!
//! That gap was not theoretical. The tiled-canvas rewrite shipped a version where nothing
//! painted at all, and no test could have caught it, because every test covered a layer
//! below this one.
//!
//! # Ordering
//!
//! A frame's ops are grouped into stroke *segments* at each `Begin`. Every segment needs its
//! own uniform writes (paint, and the per-tile records), and every write in a submission
//! lands before *any* of its commands — so a segment boundary is also a submission boundary.
//! Within a segment there is exactly one write of each, which keeps the common case at one
//! submit per frame rather than one per input sample.

use openpaint_core::{Dab, PageRect};

use crate::canvas_renderer::CanvasRenderer;
use crate::editor::StrokeOp;
use crate::history::{History, Op};
use crate::stroke_layer::StrokeLayer;
use crate::tile_store::LayerId;

/// Everything executing a stroke needs, borrowed for the duration.
pub struct StrokeExec<'a> {
    /// The layer being painted. Recorded with the stroke, so undo puts the pixels back where
    /// they came from even if the selection has moved on since.
    pub layer: LayerId,
    pub device: &'a wgpu::Device,
    pub queue: &'a wgpu::Queue,
    pub canvas: &'a mut CanvasRenderer,
    pub stroke: &'a mut StrokeLayer,
    pub history: &'a mut History,
    /// Dabs of the stroke being recorded, accumulated across frames so redo can replay it.
    pub recording: &'a mut Vec<Dab>,
    /// Paint of the stroke being recorded, captured at `Begin`.
    pub recording_paint: &'a mut ([f32; 4], f32),
}

impl StrokeExec<'_> {
    /// Run a frame's worth of stroke commands. Returns whether a stroke had to be left
    /// unrecordable because the snapshot pool was full.
    pub fn run(&mut self, ops: &[StrokeOp], dabs: &[Dab]) -> bool {
        if ops.is_empty() {
            return false;
        }

        // Upload every dab once, up front. Per-batch uploads would be clobbered: all of a
        // submission's buffer writes are applied before any of its command buffers execute.
        // See StrokeLayer::upload_dabs.
        self.stroke.upload_dabs(self.device, self.queue, dabs);
        // Dab clipping reads the page from the stroke layer's uniform, and stamping happens
        // before the frame is drawn -- so the page has to be current *here*, not at render
        // time.
        let page = self.canvas.page();
        self.stroke.set_page(self.queue, page);

        let mut unrecordable = false;
        for segment in segments(ops) {
            unrecordable |= self.run_segment(segment, dabs, page);
        }
        unrecordable
    }

    /// Execute one segment: at most one `Begin`, its stamping, and its `End`.
    fn run_segment(&mut self, ops: &[StrokeOp], dabs: &[Dab], page: PageRect) -> bool {
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("stroke-encoder"),
            });

        if let Some(StrokeOp::Begin {
            color_linear_premul,
            opacity,
        }) = ops.first()
        {
            self.recording.clear();
            *self.recording_paint = (*color_linear_premul, *opacity);
            self.stroke
                .set_paint(self.queue, *color_linear_premul, *opacity);
            self.stroke.begin_stroke();
        }

        // The union of this segment's dabs decides which accumulation tiles are needed, so
        // the per-tile records can be written once for the whole segment.
        let ranges: Vec<(usize, usize)> = ops
            .iter()
            .filter_map(|op| match *op {
                StrokeOp::Dabs { start, len } if start + len <= dabs.len() => Some((start, len)),
                _ => None,
            })
            .collect();

        if let Some(&(first, _)) = ranges.first() {
            let last = ranges.last().expect("non-empty");
            let span = &dabs[first..last.0 + last.1];
            self.recording.extend_from_slice(span);
            let tiles = self
                .stroke
                .prepare_tiles(self.queue, &mut encoder, span, page);
            for index in 0..tiles {
                for &(start, len) in &ranges {
                    self.stroke.stamp_range(&mut encoder, index, start, len);
                }
            }
        }

        let mut unrecordable = false;
        if ops.iter().any(|op| matches!(op, StrokeOp::End)) {
            unrecordable = self.commit(&mut encoder, page);
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        unrecordable
    }

    /// Snapshot what the stroke is about to overwrite, then bake it.
    fn commit(&mut self, encoder: &mut wgpu::CommandEncoder, page: PageRect) -> bool {
        let tiles = self.stroke.tiles_to_bake(page);
        if tiles.is_empty() || self.recording.is_empty() {
            self.stroke
                .bake(self.device, self.queue, encoder, self.canvas, self.layer);
            return false;
        }

        // Snapshot *before* baking: this is the pre-stroke image undo restores. A
        // GPU-to-GPU copy per tile, so nothing comes back to the CPU on the interactive
        // path.
        let before = self
            .history
            .snapshot_tiles(encoder, self.canvas, self.layer, &tiles);
        self.stroke
            .bake(self.device, self.queue, encoder, self.canvas, self.layer);

        match before {
            Some(before) => {
                let (color_linear_premul, opacity) = *self.recording_paint;
                self.history.push(Op::Stroke {
                    layer: self.layer,
                    before,
                    dabs: std::mem::take(self.recording),
                    color_linear_premul,
                    opacity,
                });
                false
            }
            None => {
                // Recording a stroke we cannot fully revert would let undo produce a state
                // that never existed. Refusing is the honest outcome, and the caller
                // surfaces it.
                self.recording.clear();
                true
            }
        }
    }
}

/// Split a frame's ops into stroke segments, each starting at a `Begin`.
///
/// A segment is the unit that needs its own uniform writes, and therefore its own
/// submission. Ops before the first `Begin` belong to the stroke already in progress.
fn segments(ops: &[StrokeOp]) -> Vec<&[StrokeOp]> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, op) in ops.iter().enumerate() {
        if i > 0 && matches!(op, StrokeOp::Begin { .. }) {
            out.push(&ops[start..i]);
            start = i;
        }
    }
    out.push(&ops[start..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use openpaint_core::Canvas;

    use crate::editor::Editor;
    use crate::test_gpu::{
        any_paint, readback_page, test_canvas, test_stroke_layer, try_device, L0,
    };

    fn begin() -> StrokeOp {
        StrokeOp::Begin {
            color_linear_premul: [0.0; 4],
            opacity: 1.0,
        }
    }

    #[test]
    fn one_stroke_is_one_segment() {
        let ops = [begin(), StrokeOp::Dabs { start: 0, len: 3 }, StrokeOp::End];
        assert_eq!(segments(&ops).len(), 1);
    }

    /// Two strokes in one frame must not share uniform writes, because the second's paint
    /// and tile records would land before the first's draws executed.
    #[test]
    fn two_strokes_in_a_frame_are_two_segments() {
        let ops = [
            begin(),
            StrokeOp::Dabs { start: 0, len: 3 },
            StrokeOp::End,
            begin(),
            StrokeOp::Dabs { start: 3, len: 2 },
        ];
        let segs = segments(&ops);
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].len(), 3);
        assert_eq!(segs[1].len(), 2);
    }

    /// Mid-stroke frames carry no `Begin` at all: those ops continue the segment that is
    /// already running, so they must not be dropped.
    #[test]
    fn ops_without_a_begin_are_one_continuing_segment() {
        let ops = [
            StrokeOp::Dabs { start: 0, len: 3 },
            StrokeOp::Dabs { start: 3, len: 4 },
        ];
        let segs = segments(&ops);
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].len(), 2);
    }

    /// The whole paint path, driven exactly as the app drives it: a real `Editor`, its
    /// command stream, this executor, then the canvas read back.
    ///
    /// This is the test that was missing. Every other GPU test calls `StrokeLayer` directly,
    /// so all of them passed while the app painted nothing at all.
    #[test]
    fn a_stroke_from_the_editor_lands_on_the_canvas() {
        let Some((device, queue)) = try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };

        let mut editor = Editor::new();
        editor.brush_mut().radius = 12.0;
        let page = editor.page_rect();

        let mut layer = test_stroke_layer(&device);

        let mut canvas = test_canvas(&device, page, &layer);
        let mut history = History::new(&device);
        let mut recording = Vec::new();
        let mut recording_paint = ([0.0; 4], 1.0);

        // Frame 1: pen down and a drag, exactly as `handle_pen_event` produces.
        editor.stroke_begin(400.0, 400.0, 1.0);
        editor.stroke_to(600.0, 420.0, 1.0);
        run_frame(
            &device,
            &queue,
            &mut canvas,
            &mut layer,
            &mut history,
            &mut recording,
            &mut recording_paint,
            &mut editor,
        );
        assert!(layer.has_paint(), "nothing accumulated on the first frame");

        // Frame 2: more of the same stroke, then pen up.
        editor.stroke_to(800.0, 500.0, 1.0);
        editor.stroke_end();
        run_frame(
            &device,
            &queue,
            &mut canvas,
            &mut layer,
            &mut history,
            &mut recording,
            &mut recording_paint,
            &mut editor,
        );

        assert!(
            canvas.layer_tiles(L0).count() > 0,
            "the bake allocated no canvas tiles"
        );
        let pixels = readback_page(&device, &queue, &canvas, L0);
        assert!(any_paint(&pixels), "the stroke did not reach the canvas");
        assert_eq!(history.undo_depth(), 1, "the stroke was not recorded");
    }

    /// A whole stroke delivered in one frame is what a quick tap produces, and it has to
    /// work without a second frame to lean on.
    #[test]
    fn a_tap_in_a_single_frame_lands_on_the_canvas() {
        let Some((device, queue)) = try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };

        let mut editor = Editor::new();
        editor.brush_mut().radius = 20.0;
        let page = editor.page_rect();
        let mut layer = test_stroke_layer(&device);
        let mut canvas = test_canvas(&device, page, &layer);
        let mut history = History::new(&device);
        let mut recording = Vec::new();
        let mut recording_paint = ([0.0; 4], 1.0);

        editor.stroke_begin(500.0, 500.0, 1.0);
        editor.stroke_end();
        run_frame(
            &device,
            &queue,
            &mut canvas,
            &mut layer,
            &mut history,
            &mut recording,
            &mut recording_paint,
            &mut editor,
        );

        let pixels = readback_page(&device, &queue, &canvas, L0);
        assert!(any_paint(&pixels), "a single-frame tap painted nothing");
    }

    /// Two separate strokes in one frame: the second must not erase the first, which is what
    /// sharing a uniform write between segments would do.
    #[test]
    fn two_strokes_in_one_frame_both_land() {
        let Some((device, queue)) = try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };

        let mut editor = Editor::new();
        editor.brush_mut().radius = 15.0;
        let page = editor.page_rect();
        let mut layer = test_stroke_layer(&device);
        let mut canvas = test_canvas(&device, page, &layer);
        let mut history = History::new(&device);
        let mut recording = Vec::new();
        let mut recording_paint = ([0.0; 4], 1.0);

        // Far apart, so each lands in its own tile and one cannot mask the other.
        editor.stroke_begin(200.0, 200.0, 1.0);
        editor.stroke_end();
        editor.stroke_begin(1600.0, 1600.0, 1.0);
        editor.stroke_end();
        run_frame(
            &device,
            &queue,
            &mut canvas,
            &mut layer,
            &mut history,
            &mut recording,
            &mut recording_paint,
            &mut editor,
        );

        let pixels = readback_page(&device, &queue, &canvas, L0);
        let at = |x: u32, y: u32| pixels[(y * page.w + x) as usize];
        let paper = Canvas::paper_color()[0];
        assert!(
            at(200, 200)[0] < paper - 0.05,
            "the first stroke is missing: {:?}",
            at(200, 200)
        );
        assert!(
            at(1600, 1600)[0] < paper - 0.05,
            "the second stroke is missing: {:?}",
            at(1600, 1600)
        );
        assert_eq!(history.undo_depth(), 2);
    }

    /// The point of residency, end to end: paint a stroke, force its tiles off the GPU by
    /// starving the pool, then bring them back and check the pixels are still there.
    ///
    /// A tiny budget rather than a huge canvas, because the failure mode is about eviction,
    /// not size -- and this keeps the test fast enough to run every time.
    #[test]
    fn paint_survives_being_spilled_and_restored() {
        let Some((device, queue)) = try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };

        let mut editor = Editor::new();
        editor.brush_mut().radius = 30.0;
        let page = editor.page_rect();
        // Room for four tiles, so a stroke across several forces eviction of its own tiles.
        let mut layer = test_stroke_layer(&device);
        let mut canvas = CanvasRenderer::new(
            &device,
            crate::test_gpu::SURFACE,
            page,
            4 * openpaint_core::tile::TILE_BYTES as u64,
            &layer,
        );
        let mut history = History::new(&device);
        let mut recording = Vec::new();
        let mut recording_paint = ([0.0; 4], 1.0);

        // A short stroke inside one tile, so it certainly fits and certainly lands.
        editor.stroke_begin(100.0, 100.0, 1.0);
        editor.stroke_to(150.0, 150.0, 1.0);
        editor.stroke_end();
        run_frame(
            &device,
            &queue,
            &mut canvas,
            &mut layer,
            &mut history,
            &mut recording,
            &mut recording_paint,
            &mut editor,
        );
        assert!(
            canvas.slot(L0, (0, 0)).is_some(),
            "tile (0,0) should be resident"
        );

        // Now paint far away, over enough new tiles that (0,0) has to be evicted. Each stroke
        // is its own frame, because a tile touched in the current frame is deliberately not an
        // eviction candidate.
        for i in 0..6 {
            canvas.begin_frame();
            let x = 700.0 + i as f32 * 300.0;
            editor.stroke_begin(x, 700.0, 1.0);
            editor.stroke_to(x + 40.0, 740.0, 1.0);
            editor.stroke_end();
            run_frame(
                &device,
                &queue,
                &mut canvas,
                &mut layer,
                &mut history,
                &mut recording,
                &mut recording_paint,
                &mut editor,
            );
        }
        assert!(
            canvas.slot(L0, (0, 0)).is_none(),
            "tile (0,0) should have been evicted by now"
        );
        assert!(canvas.traffic().0 > 0, "nothing was ever read back");

        // Ask for it back and confirm the paint is intact.
        canvas.begin_frame();
        let mut enc = device.create_command_encoder(&Default::default());
        assert!(
            canvas
                .make_resident(&device, &queue, &mut enc, L0, (0, 0))
                .is_some(),
            "the spilled tile could not be restored"
        );
        queue.submit(std::iter::once(enc.finish()));

        // Just this tile: most of the canvas is spilled on purpose, so a whole-page readback
        // would (correctly) refuse.
        let tile = crate::test_gpu::readback_tile(&device, &queue, &canvas, L0, (0, 0))
            .expect("resident after the restore");
        let texel = tile[125 * openpaint_core::tile::TILE_SIZE + 125];
        assert!(
            texel[3] > 0.05,
            "the stroke did not survive the round trip: {texel:?}"
        );
    }

    /// Drain the editor's pending commands through the executor, as `redraw` does.
    #[allow(clippy::too_many_arguments)]
    fn run_frame(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        canvas: &mut CanvasRenderer,
        layer: &mut StrokeLayer,
        history: &mut History,
        recording: &mut Vec<Dab>,
        recording_paint: &mut ([f32; 4], f32),
        editor: &mut Editor,
    ) {
        assert!(editor.has_pending_stroke(), "nothing queued to execute");
        {
            let (ops, dabs) = editor.pending_stroke();
            let mut exec = StrokeExec {
                layer: L0,
                device,
                queue,
                canvas,
                stroke: layer,
                history,
                recording,
                recording_paint,
            };
            exec.run(ops, dabs);
        }
        editor.clear_pending_stroke();
    }
}
