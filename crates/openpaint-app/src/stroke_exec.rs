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
    pub recording_paint: &'a mut ([f32; 4], f32, crate::editor::PaintMode),
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
            mode,
        }) = ops.first()
        {
            self.recording.clear();
            *self.recording_paint = (*color_linear_premul, *opacity, *mode);
            self.stroke
                .set_paint(self.queue, *color_linear_premul, *opacity, *mode);
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
                let (color_linear_premul, opacity, mode) = *self.recording_paint;
                self.history.push(Op::Paint {
                    layer: self.layer,
                    before,
                    source: crate::history::PaintSource::Dabs(std::mem::take(self.recording)),
                    color_linear_premul,
                    opacity,
                    mode,
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
    /// Opaque red, premultiplied.
    const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];

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
            mode: crate::editor::PaintMode::Normal,
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
        let mut recording_paint = ([0.0; 4], 1.0, crate::editor::PaintMode::Normal);

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
        let mut recording_paint = ([0.0; 4], 1.0, crate::editor::PaintMode::Normal);

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
        let mut recording_paint = ([0.0; 4], 1.0, crate::editor::PaintMode::Normal);

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
        let mut recording_paint = ([0.0; 4], 1.0, crate::editor::PaintMode::Normal);

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

    /// What the screen shows *mid-stroke*: the committed artwork must still be there, and the
    /// stroke in progress must be visible on top of it.
    ///
    /// The preview had no test at all, which is how it shipped broken. Everything else stops at
    /// "the tile holds the right pixels" or "a committed stroke reaches the screen"; neither
    /// says anything about the frame drawn while the pen is still down.
    #[test]
    fn a_stroke_in_progress_shows_over_the_committed_artwork() {
        let Some((device, queue)) = try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };
        const VIEW: u32 = 512;

        let mut editor = Editor::new();
        editor.brush_mut().radius = 40.0;
        let page = editor.page_rect();
        let mut layer = test_stroke_layer(&device);
        let mut canvas = test_canvas(&device, page, &layer);
        let mut history = History::new(&device);
        let mut recording = Vec::new();
        let mut recording_paint = ([0.0; 4], 1.0, crate::editor::PaintMode::Normal);

        // A committed stroke on the left.
        editor.stroke_begin(300.0, 500.0, 1.0);
        editor.stroke_to(500.0, 500.0, 1.0);
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

        // A second stroke still in progress -- no `stroke_end`. It starts *over* the committed
        // one and runs off onto fresh ground, so one run covers both cases: a tile the canvas
        // already has, and a tile it has never had.
        editor.stroke_begin(400.0, 500.0, 1.0);
        editor.stroke_to(900.0, 500.0, 1.0);
        editor.stroke_to(1500.0, 500.0, 1.0);
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
        assert!(layer.has_paint(), "the stroke should be accumulating");

        let doc = openpaint_core::Page::new(page.w, page.h);
        let mut view = crate::view::View::new();
        view.fit(VIEW, VIEW, page);
        canvas.begin_frame();
        let mut enc = device.create_command_encoder(&Default::default());
        canvas.prepare(
            &device,
            &queue,
            &mut enc,
            view.page_to_ndc(VIEW, VIEW),
            view.visible_rect(VIEW, VIEW),
            doc.layers(),
            0,
            Some(&layer),
        );
        queue.submit(std::iter::once(enc.finish()));
        let screen =
            crate::canvas_renderer::tests::draw_to_target(&device, &queue, &canvas, VIEW, VIEW);

        let at = |px: f32, py: f32| {
            let (sx, sy) = view.canvas_to_screen(px, py, VIEW, VIEW);
            screen[(sy.round() as usize) * VIEW as usize + (sx.round() as usize)]
        };
        // Paper is bright; a black stroke on it is dark. Nothing here should be brighter than
        // paper, which is what a runaway preview looks like.
        let paper = at(1000.0, 1000.0);
        assert!(paper[0] > 200, "the sheet should be paper: {paper:?}");

        // Part of the committed stroke the new one does not cover.
        let committed = at(320.0, 500.0);
        assert!(
            committed[0] < 100,
            "the committed stroke vanished while drawing: {committed:?}"
        );

        // Where the two overlap: still paint, not a hole.
        let overlap = at(450.0, 500.0);
        assert!(
            overlap[0] < 100,
            "the overlap went blank while drawing: {overlap:?}"
        );

        let in_progress = at(1400.0, 500.0);
        assert!(
            in_progress[0] < 100,
            "the stroke in progress is not visible: {in_progress:?}"
        );

        // And nowhere may be *brighter* than paper: a white box is the symptom of the preview
        // blowing up rather than blending.
        let brightest = screen.iter().map(|p| p[0]).max().unwrap_or(0);
        assert!(
            brightest <= paper[0].saturating_add(3),
            "something is brighter than paper ({brightest} vs {}), i.e. a white box",
            paper[0]
        );
    }

    /// Paint, save, and load back through the real path: a stroke on the GPU, read out of the
    /// tile store, written to a file, read in, and compared pixel for pixel.
    ///
    /// The format crate's own tests use synthetic tile bytes, which proves the container works but
    /// says nothing about whether the *app* hands it the right tiles or puts them back in the
    /// right place. That seam is where a save silently loses work, so it gets its own test.
    #[test]
    fn a_painted_document_survives_a_save_and_load() {
        let Some((device, queue)) = try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };

        let mut editor = Editor::new();
        editor.brush_mut().radius = 30.0;
        let page = editor.page_rect();
        let mut layer = test_stroke_layer(&device);
        let mut canvas = test_canvas(&device, page, &layer);
        let mut history = History::new(&device);
        let mut recording = Vec::new();
        let mut recording_paint = ([0.0; 4], 1.0, crate::editor::PaintMode::Normal);

        // Two layers with a stroke each, so the save has to keep them apart.
        editor.stroke_begin(400.0, 400.0, 1.0);
        editor.stroke_to(700.0, 450.0, 1.0);
        editor.stroke_end();
        let bottom_id = editor.active_layer_id();
        run_frame_on(
            &device,
            &queue,
            &mut canvas,
            &mut layer,
            &mut history,
            &mut recording,
            &mut recording_paint,
            &mut editor,
            bottom_id,
        );

        editor.document_mut().add_layer();
        editor.document_mut().active_mut().active_layer_mut().blend =
            openpaint_core::Blend::Multiply;
        editor
            .document_mut()
            .active_mut()
            .active_layer_mut()
            .opacity = 0.6;
        let top_id = editor.active_layer_id();
        assert_ne!(top_id, bottom_id);
        editor.stroke_begin(1200.0, 1200.0, 1.0);
        editor.stroke_to(1500.0, 1250.0, 1.0);
        editor.stroke_end();
        run_frame_on(
            &device,
            &queue,
            &mut canvas,
            &mut layer,
            &mut history,
            &mut recording,
            &mut recording_paint,
            &mut editor,
            top_id,
        );

        // What is on the GPU right now, to compare against.
        let before: std::collections::HashMap<_, _> = canvas
            .snapshot_all(&device, &queue)
            .into_iter()
            .map(|(k, t)| (k, t.bytes().to_vec()))
            .collect();
        assert!(before.len() >= 2, "expected tiles on both layers");

        let path = std::env::temp_dir().join(format!(
            "openpaint-app-roundtrip-{}.openpaint",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);

        // Save exactly as the app does.
        let mut page_of_layer = std::collections::HashMap::new();
        for i in 0..editor.document().page_count() {
            if let Some(p) = editor.document().page(i) {
                for l in p.layers() {
                    page_of_layer.insert(l.id(), i);
                }
            }
        }
        let refs = canvas
            .snapshot_all(&device, &queue)
            .into_iter()
            .filter_map(|(key, tile)| {
                let page = *page_of_layer.get(&key.layer.0)?;
                Some((
                    openpaint_file::TileRef {
                        page,
                        layer_id: key.layer.0,
                        coord: key.coord,
                    },
                    tile,
                ))
            });
        openpaint_file::save(&path, editor.document(), refs, &[]).expect("save");

        let loaded = openpaint_file::load(&path).expect("load");
        let _ = std::fs::remove_file(&path);

        // Structure: the stack, its ids, and the properties that decide how it looks.
        let a = editor.document().active();
        let b = loaded.document.active();
        assert_eq!(a.layer_count(), b.layer_count());
        for (x, y) in a.layers().iter().zip(b.layers()) {
            assert_eq!(x, y, "layer {:?} changed across the save", x.name);
        }

        // Pixels: every tile back, keyed to the same layer, byte for byte.
        assert_eq!(loaded.tiles.len(), before.len(), "tile count changed");
        for (key, bytes) in &before {
            let r = openpaint_file::TileRef {
                page: 0,
                layer_id: key.layer.0,
                coord: key.coord,
            };
            let got = loaded
                .tiles
                .get(&r)
                .unwrap_or_else(|| panic!("{r:?} missing after load"));
            assert_eq!(got.bytes(), bytes.as_slice(), "{r:?} came back different");
        }
    }

    /// Erasing must remove coverage, not paint the paper colour over it.
    ///
    /// The distinction is the whole point and it is invisible on a single layer over paper, where
    /// both look identical. So this puts the erased layer *above* another one: only true removal
    /// reveals what is beneath.
    #[test]
    fn erasing_removes_coverage_rather_than_painting_over_it() {
        let Some((device, queue)) = try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };

        let mut editor = Editor::new();
        editor.brush_mut().radius = 40.0;
        editor.brush_mut().hardness = 1.0;
        let page = editor.page_rect();
        let mut layer = test_stroke_layer(&device);
        let mut canvas = test_canvas(&device, page, &layer);
        let mut history = History::new(&device);
        let mut recording = Vec::new();
        let mut recording_paint = ([0.0; 4], 1.0, crate::editor::PaintMode::Normal);

        // Bottom layer: paint across the area.
        let bottom = editor.active_layer_id();
        editor.stroke_begin(400.0, 400.0, 1.0);
        editor.stroke_to(800.0, 400.0, 1.0);
        editor.stroke_end();
        run_frame_on(
            &device,
            &queue,
            &mut canvas,
            &mut layer,
            &mut history,
            &mut recording,
            &mut recording_paint,
            &mut editor,
            bottom,
        );

        // Top layer: paint over the same area, then erase part of it.
        editor.document_mut().add_layer();
        let top = editor.active_layer_id();
        editor.stroke_begin(400.0, 400.0, 1.0);
        editor.stroke_to(800.0, 400.0, 1.0);
        editor.stroke_end();
        run_frame_on(
            &device,
            &queue,
            &mut canvas,
            &mut layer,
            &mut history,
            &mut recording,
            &mut recording_paint,
            &mut editor,
            top,
        );

        let painted =
            crate::test_gpu::readback_tile(&device, &queue, &canvas, LayerId(top), (2, 1))
                .expect("resident");
        let at = |t: &Vec<[f32; 4]>, x: usize, y: usize| {
            t[(y % openpaint_core::tile::TILE_SIZE) * openpaint_core::tile::TILE_SIZE
                + (x % openpaint_core::tile::TILE_SIZE)]
        };
        assert!(
            at(&painted, 600, 400)[3] > 0.9,
            "the top layer should be opaque there: {:?}",
            at(&painted, 600, 400)
        );

        editor.set_tool(crate::editor::Tool::Eraser);
        editor.brush_mut().radius = 40.0;
        editor.brush_mut().hardness = 1.0;
        editor.stroke_begin(600.0, 400.0, 1.0);
        editor.stroke_to(620.0, 400.0, 1.0);
        editor.stroke_end();
        run_frame_on(
            &device,
            &queue,
            &mut canvas,
            &mut layer,
            &mut history,
            &mut recording,
            &mut recording_paint,
            &mut editor,
            top,
        );

        let erased = crate::test_gpu::readback_tile(&device, &queue, &canvas, LayerId(top), (2, 1))
            .expect("resident");
        let hole = at(&erased, 610, 400);
        assert!(
            hole[3] < 0.05,
            "the erase left coverage behind, so it painted rather than removed: {hole:?}"
        );
        // Premultiplied: removing coverage must take the colour with it, or the hole keeps a
        // ghost that shows up the moment anything is composited under it.
        assert!(
            hole[0].abs() < 0.05 && hole[1].abs() < 0.05 && hole[2].abs() < 0.05,
            "colour survived the erase: {hole:?}"
        );

        // Away from the erase the top layer is untouched.
        let kept = at(&erased, 450, 400);
        assert!(kept[3] > 0.9, "the erase spread too far: {kept:?}");

        // And the layer *below* still has its paint -- the erase must not have reached it.
        let under =
            crate::test_gpu::readback_tile(&device, &queue, &canvas, LayerId(bottom), (2, 1))
                .expect("resident");
        assert!(
            at(&under, 610, 400)[3] > 0.9,
            "the erase went through to the layer below: {:?}",
            at(&under, 610, 400)
        );
    }

    /// Redo has to replay an erase as an erase. It replays dabs rather than storing an after-image,
    /// so an unrecorded mode would come back as a black stroke -- the worst kind of undo bug,
    /// because it looks like it worked.
    #[test]
    fn redoing_an_erase_erases_again() {
        let Some((device, queue)) = try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };

        let mut editor = Editor::new();
        editor.brush_mut().radius = 40.0;
        editor.brush_mut().hardness = 1.0;
        let page = editor.page_rect();
        let mut layer = test_stroke_layer(&device);
        let mut canvas = test_canvas(&device, page, &layer);
        let mut history = History::new(&device);
        let mut recording = Vec::new();
        let mut recording_paint = ([0.0; 4], 1.0, crate::editor::PaintMode::Normal);
        let id = editor.active_layer_id();

        editor.stroke_begin(400.0, 400.0, 1.0);
        editor.stroke_to(800.0, 400.0, 1.0);
        editor.stroke_end();
        run_frame_on(
            &device,
            &queue,
            &mut canvas,
            &mut layer,
            &mut history,
            &mut recording,
            &mut recording_paint,
            &mut editor,
            id,
        );

        editor.set_tool(crate::editor::Tool::Eraser);
        editor.brush_mut().radius = 40.0;
        editor.brush_mut().hardness = 1.0;
        editor.stroke_begin(600.0, 400.0, 1.0);
        editor.stroke_to(620.0, 400.0, 1.0);
        editor.stroke_end();
        run_frame_on(
            &device,
            &queue,
            &mut canvas,
            &mut layer,
            &mut history,
            &mut recording,
            &mut recording_paint,
            &mut editor,
            id,
        );

        // The op history recorded it; replaying must reproduce a hole, not a stroke.
        let op = history.pop_undo().expect("the erase was recorded");
        match &op {
            Op::Paint { mode, .. } => assert_eq!(
                *mode,
                crate::editor::PaintMode::Erase,
                "the erase was recorded as paint"
            ),
            _ => panic!("the last operation was not a stroke"),
        }
    }

    /// **The eraser is the same brush.** Every brush property must reach it identically, and not
    /// because someone remembered to wire each one up twice.
    ///
    /// Pinned as an exact identity rather than a spot check. Paint a stroke onto a transparent
    /// layer and it *adds* coverage; erase the same stroke from an opaque layer and it *removes*
    /// the same coverage. So for every pixel:
    ///
    /// ```text
    /// alpha(painted) + alpha(erased from opaque) == 1
    /// ```
    ///
    /// That holds for any radius, hardness, flow, spacing, pressure or opacity, because all of
    /// them act on the accumulated coverage and nothing else -- and it keeps holding for
    /// properties that do not exist yet, which is the actual point. A future brush feature
    /// honoured on the paint path but forgotten on the erase path breaks this test.
    #[test]
    fn every_brush_property_reaches_the_eraser() {
        let Some((device, queue)) = try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };
        const SIDE: u32 = 300;

        // radius, hardness, flow, opacity, spacing -- deliberately awkward values, including a
        // flow and opacity below 1 so the ceiling and the build-up are both in play.
        let settings = [
            (30.0_f32, 1.0_f32, 1.0_f32, 1.0_f32, 0.25_f32),
            (18.0, 0.0, 1.0, 1.0, 0.25),
            (24.0, 0.5, 0.35, 0.6, 0.1),
            (8.0, 0.75, 0.8, 0.45, 0.5),
        ];

        for (radius, hardness, flow, opacity, spacing) in settings {
            let mut editor = Editor::new();
            editor.resize_page(openpaint_core::PageRect::from_size(SIDE, SIDE));
            let page = editor.page_rect();

            let mut layer = test_stroke_layer(&device);
            let mut canvas = test_canvas(&device, page, &layer);
            let mut history = History::new(&device);
            let mut recording = Vec::new();
            let mut recording_paint = ([0.0; 4], 1.0, crate::editor::PaintMode::Normal);

            let painted_id = editor.active_layer_id();
            editor.document_mut().add_layer();
            let erased_id = editor.active_layer_id();

            // The layer the eraser works on starts fully opaque, so what it removes is visible as
            // what is left.
            {
                let mut cpu = openpaint_core::Canvas::new(SIDE, SIDE);
                for y in 0..SIDE as i32 {
                    for x in 0..SIDE as i32 {
                        cpu.replace_pixel(x, y, [1.0, 1.0, 1.0, 1.0]);
                    }
                }
                let mut enc = device.create_command_encoder(&Default::default());
                canvas.upload_dirty(&device, &queue, &mut enc, LayerId(erased_id), &mut cpu);
                queue.submit(std::iter::once(enc.finish()));
            }

            // Identical settings on both tools, and identical geometry: the same calls, so the
            // same dabs.
            let apply = |b: &mut openpaint_core::Brush| {
                b.radius = radius;
                b.hardness = hardness;
                b.flow = flow;
                b.opacity = opacity;
                b.spacing = spacing;
            };

            for (tool, target) in [
                (crate::editor::Tool::Brush, painted_id),
                (crate::editor::Tool::Eraser, erased_id),
            ] {
                editor.set_tool(tool);
                apply(editor.brush_mut());
                // Pressure varies along the stroke, so the pressure response is in play too.
                editor.stroke_begin(60.0, 120.0, 0.4);
                editor.stroke_to(150.0, 170.0, 0.9);
                editor.stroke_to(240.0, 120.0, 0.6);
                editor.stroke_end();
                run_frame_on(
                    &device,
                    &queue,
                    &mut canvas,
                    &mut layer,
                    &mut history,
                    &mut recording,
                    &mut recording_paint,
                    &mut editor,
                    target,
                );
            }

            let painted = crate::test_gpu::readback_tile(
                &device,
                &queue,
                &canvas,
                LayerId(painted_id),
                (0, 0),
            )
            .expect("the painted layer should be resident");
            let erased = crate::test_gpu::readback_tile(
                &device,
                &queue,
                &canvas,
                LayerId(erased_id),
                (0, 0),
            )
            .expect("the erased layer should be resident");

            let mut touched = false;
            for y in 0..SIDE as usize {
                for x in 0..SIDE as usize {
                    let i = y * openpaint_core::tile::TILE_SIZE + x;
                    if x >= openpaint_core::tile::TILE_SIZE || y >= openpaint_core::tile::TILE_SIZE
                    {
                        continue;
                    }
                    let added = painted[i][3];
                    let left = erased[i][3];
                    if added > 0.02 {
                        touched = true;
                    }
                    assert!(
                        (added + left - 1.0).abs() < 0.02,
                        "at ({x}, {y}) with radius {radius} hardness {hardness} flow {flow} \
                         opacity {opacity} spacing {spacing}: brush added {added} but eraser \
                         left {left}; they should sum to 1"
                    );
                }
            }
            assert!(
                touched,
                "the stroke painted nothing, so the test proved nothing"
            );
        }
    }

    /// A fill lands inside the mask, stops at its edge, and follows its coverage.
    ///
    /// Three properties, and the third is the reason a selection is a byte per pixel rather than a
    /// bit: a partially selected pixel must fill partially. A boolean mask could not express that,
    /// and a fill that hard-edged every selection would make feathering pointless before it exists.
    #[test]
    fn a_fill_covers_the_mask_and_stops_at_its_edge() {
        let Some((device, queue)) = try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };
        const SIDE: u32 = 200;

        let mut editor = Editor::new();
        editor.resize_page(openpaint_core::PageRect::from_size(SIDE, SIDE));
        let page = editor.page_rect();
        let layer = test_stroke_layer(&device);
        let mut canvas = test_canvas(&device, page, &layer);
        let mut stroke = test_stroke_layer(&device);
        let mut history = History::new(&device);
        let id = LayerId(editor.active_layer_id());

        // A rectangle, then a manual half-coverage stripe down one column of it, so the same fill
        // exercises full and partial coverage at once.
        let mut selection = openpaint_core::Selection::from_rect(
            openpaint_core::PageRect::new(50, 50, 100, 100),
            page,
        );
        selection.set_coverage(120, 100, 128);

        let mut encoder = device.create_command_encoder(&Default::default());
        stroke.set_page(&queue, page);
        stroke.set_paint(&queue, RED, 1.0, crate::editor::PaintMode::Normal);
        stroke.begin_stroke();
        stroke.fill_from_mask(&device, &queue, &mut encoder, &selection, page);
        let tiles = stroke.tiles_to_bake(page);
        let before = history.snapshot_tiles(&mut encoder, &canvas, id, &tiles);
        stroke.bake(&device, &queue, &mut encoder, &mut canvas, id);
        queue.submit(std::iter::once(encoder.finish()));
        assert!(before.is_some(), "the fill should have been recordable");

        let after = crate::test_gpu::readback_tile(&device, &queue, &canvas, id, (0, 0))
            .expect("the filled layer should be resident");
        let at = |x: usize, y: usize| after[y * openpaint_core::tile::TILE_SIZE + x];

        assert!(
            at(100, 100)[3] > 0.99,
            "the middle of the selection is unfilled: {:?}",
            at(100, 100)
        );
        assert!(
            at(40, 100)[3] < 0.01,
            "paint escaped the selection: {:?}",
            at(40, 100)
        );
        assert!(
            at(160, 100)[3] < 0.01,
            "paint escaped the other side: {:?}",
            at(160, 100)
        );

        let partial = at(120, 100)[3];
        assert!(
            (partial - 0.5).abs() < 0.02,
            "a half-selected pixel filled at {partial} instead of about half; coverage is being \
             treated as a boolean"
        );
    }

    /// Clearing a selection removes coverage rather than painting over it.
    ///
    /// The distinction is invisible on a single layer against paper — painting white and erasing
    /// both look white — so this puts the cleared layer *above* another one. Only genuine removal
    /// reveals what is underneath.
    ///
    /// It also exercises the path `Delete` uses, which is the same fill with `PaintMode::Erase`.
    #[test]
    fn clearing_a_selection_removes_coverage() {
        let Some((device, queue)) = try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };
        const SIDE: u32 = 200;

        let mut editor = Editor::new();
        editor.resize_page(openpaint_core::PageRect::from_size(SIDE, SIDE));
        editor.document_mut().add_layer();
        let page = editor.page_rect();
        let layer_ids: Vec<u32> = editor
            .layers()
            .iter()
            .map(openpaint_core::Layer::id)
            .collect();
        let (under, over) = (LayerId(layer_ids[0]), LayerId(layer_ids[1]));

        let stroke_probe = test_stroke_layer(&device);
        let mut canvas = test_canvas(&device, page, &stroke_probe);
        let mut stroke = test_stroke_layer(&device);
        let mut history = History::new(&device);

        // Both layers opaque, so anything visible on the lower one after the clear got there by
        // removal and not by painting.
        for id in [under, over] {
            let mut cpu = openpaint_core::Canvas::new(SIDE, SIDE);
            for y in 0..SIDE as i32 {
                for x in 0..SIDE as i32 {
                    cpu.replace_pixel(x, y, [0.4, 0.4, 0.4, 1.0]);
                }
            }
            let mut enc = device.create_command_encoder(&Default::default());
            canvas.upload_dirty(&device, &queue, &mut enc, id, &mut cpu);
            queue.submit(std::iter::once(enc.finish()));
        }

        let selection = openpaint_core::Selection::from_rect(
            openpaint_core::PageRect::new(50, 50, 60, 60),
            page,
        );

        let mut encoder = device.create_command_encoder(&Default::default());
        stroke.set_page(&queue, page);
        stroke.set_paint(&queue, [0.0; 4], 1.0, crate::editor::PaintMode::Erase);
        stroke.begin_stroke();
        stroke.fill_from_mask(&device, &queue, &mut encoder, &selection, page);
        let tiles = stroke.tiles_to_bake(page);
        let before = history.snapshot_tiles(&mut encoder, &canvas, over, &tiles);
        stroke.bake(&device, &queue, &mut encoder, &mut canvas, over);
        queue.submit(std::iter::once(encoder.finish()));
        assert!(before.is_some(), "the clear should have been recordable");

        let after = crate::test_gpu::readback_tile(&device, &queue, &canvas, over, (0, 0))
            .expect("the cleared layer should be resident");
        let at = |x: usize, y: usize| after[y * openpaint_core::tile::TILE_SIZE + x];

        let hole = at(80, 80);
        assert!(
            hole[3] < 0.01,
            "the clear left coverage behind, so it painted rather than removed: {hole:?}"
        );
        assert!(hole[0] < 0.01, "colour survived the clear: {hole:?}");

        let kept = at(20, 20);
        assert!(
            kept[3] > 0.99,
            "the clear spread outside the selection: {kept:?}"
        );
    }

    /// Alpha lock means alpha cannot change. Not "mostly", not "except at the edges".
    ///
    /// Paints a half-covered patch, locks the layer, then scrubs a much larger stroke of a different
    /// colour across it and past its edges. Three things have to hold:
    ///
    /// - **every pixel keeps its exact alpha**, inside the patch, outside it, and across the soft
    ///   boundary where a masking implementation would leak;
    /// - **colour inside the patch changes**, or the lock has simply disabled painting;
    /// - **nothing appears outside the patch**, which is the whole point.
    ///
    /// The patch is deliberately at partial alpha: with an opaque patch, "preserved alpha" and
    /// "wrote 1.0" are indistinguishable, and that is exactly the bug a source-atop blend could
    /// have.
    #[test]
    fn alpha_lock_cannot_change_coverage() {
        let Some((device, queue)) = try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };
        const SIDE: u32 = 200;

        let mut editor = Editor::new();
        editor.resize_page(openpaint_core::PageRect::from_size(SIDE, SIDE));
        let page = editor.page_rect();
        let mut layer = test_stroke_layer(&device);
        let mut canvas = test_canvas(&device, page, &layer);
        let mut history = History::new(&device);
        let mut recording = Vec::new();
        let mut recording_paint = ([0.0; 4], 1.0, crate::editor::PaintMode::Normal);
        let id = editor.active_layer_id();

        // A half-covered square in the middle, uploaded directly so the starting alpha is exact.
        let mut cpu = openpaint_core::Canvas::new(SIDE, SIDE);
        for y in 60..140 {
            for x in 60..140 {
                // Premultiplied red at alpha 0.5.
                cpu.replace_pixel(x, y, [0.5, 0.0, 0.0, 0.5]);
            }
        }
        {
            let mut enc = device.create_command_encoder(&Default::default());
            canvas.upload_dirty(&device, &queue, &mut enc, LayerId(id), &mut cpu);
            queue.submit(std::iter::once(enc.finish()));
        }
        let before = crate::test_gpu::readback_tile(&device, &queue, &canvas, LayerId(id), (0, 0))
            .expect("the layer should be resident");

        // Lock it, then scrub right across the patch and well beyond it.
        editor
            .document_mut()
            .active_mut()
            .layer_mut(0)
            .expect("the layer")
            .lock_alpha = true;
        assert_eq!(
            editor.paint_mode(),
            Some(crate::editor::PaintMode::LockAlpha),
            "the layer flag has to reach the paint mode, or this test proves nothing"
        );
        editor.brush_mut().radius = 30.0;
        editor.brush_mut().hardness = 1.0;
        editor.brush_mut().set_color_srgb8([0, 0, 255]);
        editor.stroke_begin(20.0, 100.0, 1.0);
        editor.stroke_to(180.0, 100.0, 1.0);
        editor.stroke_end();
        run_frame_on(
            &device,
            &queue,
            &mut canvas,
            &mut layer,
            &mut history,
            &mut recording,
            &mut recording_paint,
            &mut editor,
            id,
        );

        let after = crate::test_gpu::readback_tile(&device, &queue, &canvas, LayerId(id), (0, 0))
            .expect("the layer should still be resident");

        let at = |t: &Vec<[f32; 4]>, x: usize, y: usize| t[y * openpaint_core::tile::TILE_SIZE + x];

        // 1. Alpha is untouched everywhere the stroke went, including across the patch boundary.
        let mut checked = 0;
        for x in 20..180 {
            for y in 85..115 {
                let a0 = at(&before, x, y)[3];
                let a1 = at(&after, x, y)[3];
                assert!(
                    (a1 - a0).abs() < 0.004,
                    "alpha changed at ({x}, {y}): {a0} -> {a1}"
                );
                checked += 1;
            }
        }
        assert!(checked > 4000, "the sweep covered almost nothing");

        // 2. Colour inside the patch really did change, or the lock just disabled the brush.
        let inside_before = at(&before, 100, 100);
        let inside_after = at(&after, 100, 100);
        assert!(
            inside_after[2] > inside_before[2] + 0.1,
            "no blue arrived inside the locked patch: {inside_before:?} -> {inside_after:?}"
        );

        // 3. And nothing appeared outside it.
        let outside = at(&after, 30, 100);
        assert!(
            outside[3] < 0.004 && outside[2] < 0.004,
            "paint escaped onto transparent pixels: {outside:?}"
        );
    }

    /// Erasing on an alpha-locked layer is refused rather than performed as an invisible no-op.
    #[test]
    fn erasing_a_locked_layer_is_refused() {
        let mut editor = Editor::new();
        editor
            .document_mut()
            .active_mut()
            .layer_mut(0)
            .expect("the layer")
            .lock_alpha = true;
        editor.set_tool(crate::editor::Tool::Eraser);

        assert_eq!(
            editor.paint_mode(),
            None,
            "alpha lock forbids changing alpha, and erasing is nothing else"
        );
        editor.stroke_begin(10.0, 10.0, 1.0);
        assert!(
            !editor.is_drawing(),
            "a stroke that cannot do anything must not start"
        );
        assert!(
            !editor.has_pending_stroke(),
            "and must queue no work for the renderer"
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
        recording_paint: &mut ([f32; 4], f32, crate::editor::PaintMode),
        editor: &mut Editor,
    ) {
        run_frame_on(
            device,
            queue,
            canvas,
            layer,
            history,
            recording,
            recording_paint,
            editor,
            0,
        );
    }

    /// As `run_frame`, but painting on a chosen layer id rather than assuming the first.
    #[allow(clippy::too_many_arguments)]
    fn run_frame_on(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        canvas: &mut CanvasRenderer,
        layer: &mut StrokeLayer,
        history: &mut History,
        recording: &mut Vec<Dab>,
        recording_paint: &mut ([f32; 4], f32, crate::editor::PaintMode),
        editor: &mut Editor,
        layer_id: u32,
    ) {
        assert!(editor.has_pending_stroke(), "nothing queued to execute");
        {
            let (ops, dabs) = editor.pending_stroke();
            let mut exec = StrokeExec {
                layer: LayerId(layer_id),
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
