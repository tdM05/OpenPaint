//! GPU context and frame presentation.
//!
//! Deliberately narrow: this owns wgpu resources and draws what it is given. It
//! does **not** own the document, the brush, the stroke state, or the UI — those
//! are [`crate::editor::Editor`], [`crate::view::View`], and `crate::ui`
//! respectively.
//!
//! That separation is not tidiness for its own sake. This type previously held all
//! of them, which meant every new feature (pan/zoom, pages, layers, undo) would
//! have landed on one struct that was simultaneously the renderer, the document
//! owner, and the stroke state machine.
//!
//! # The UI seam
//!
//! Anything drawing on top of the canvas arrives as a closure taking an
//! [`Overlay`], so this module has no dependency on egui or on any UI library.
//! Swapping the UI (Q4 is still open, and egui is explicitly throwaway) touches
//! the callback, not the renderer.

use std::sync::Arc;

use openpaint_core::Canvas;
use winit::window::Window;

use crate::canvas_renderer::{CanvasRenderer, CANVAS_FORMAT};
use crate::editor::StrokeOp;
use crate::history::{History, Op, TileBefore};
use crate::stroke_layer::StrokeLayer;
use crate::tile_pool::TilePool;
use crate::view::PageToNdc;
use openpaint_core::{PageRect, PageResize};

/// What an undo or redo did, and therefore what the caller must reconcile.
///
/// Geometry changes cannot be handled entirely in here: the page's dimensions live in
/// the editor while its pixels live on the GPU, so the shell has to apply both halves.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HistoryChange {
    /// Nothing to undo or redo.
    None,
    /// Pixels changed; just redraw.
    Pixels,
    /// The page's rectangle changed; the editor's page must be moved to match.
    Geometry { rect: openpaint_core::PageRect },
}

/// Everything an overlay needs to draw itself into the current frame.
pub struct Overlay<'a> {
    pub device: &'a wgpu::Device,
    pub queue: &'a wgpu::Queue,
    pub encoder: &'a mut wgpu::CommandEncoder,
    /// The frame's color target, already containing the canvas.
    pub target: &'a wgpu::TextureView,
    /// Surface size in physical pixels.
    pub size_px: [u32; 2],
}

pub struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    canvas_renderer: CanvasRenderer,
    /// GPU dab rasterization and the in-progress stroke.
    stroke_layer: StrokeLayer,
    /// Undo/redo. GPU-side because the GPU owns the pixels (see `crate::history`).
    history: History,
    /// Dabs of the stroke being recorded, accumulated across frames so redo can
    /// replay it. A stroke spans many frames, and each frame's dabs are cleared
    /// once executed, so history has to keep its own copy.
    recording: Vec<openpaint_core::Dab>,
    /// Paint of the stroke being recorded, captured at Begin.
    recording_paint: ([f32; 4], f32),
    /// Set when a stroke could not be recorded for undo because the snapshot pool was
    /// full even after evicting everything.
    unrecordable: bool,
    window: Arc<Window>,
}

impl Renderer {
    /// Create the GPU context for a window. Blocks on async setup via pollster.
    pub fn new(window: Arc<Window>, canvas: &Canvas) -> Result<Self, String> {
        let size = window.inner_size();

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            ..Default::default()
        });

        let surface = instance
            .create_surface(window.clone())
            .map_err(|e| format!("create_surface failed: {e}"))?;

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .ok_or_else(|| "no suitable GPU adapter found".to_string())?;

        let info = adapter.get_info();
        println!(
            "GPU: {} ({:?}) via {:?}",
            info.name, info.device_type, info.backend
        );

        // Default limits (not downlevel-relaxed, not raised): the target is a
        // Surface-class integrated GPU, so staying at defaults keeps us portable.
        // See DECISIONS §2.
        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("openpaint-device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::Performance,
            },
            None,
        ))
        .map_err(|e| format!("request_device failed: {e}"))?;

        let caps = surface.get_capabilities(&adapter);
        // Prefer an sRGB surface so wgpu encodes on write and we can work in
        // linear throughout (DECISIONS §4b).
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        let canvas_renderer = CanvasRenderer::new(&device, format, canvas);
        let stroke_layer = StrokeLayer::new(&device, CANVAS_FORMAT, format);
        let history = History::new(&device);

        Ok(Self {
            surface,
            device,
            queue,
            config,
            canvas_renderer,
            stroke_layer,
            history,
            recording: Vec::new(),
            recording_paint: ([0.0; 4], 1.0),
            unrecordable: false,
            window,
        })
    }

    #[must_use]
    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    #[must_use]
    pub fn surface_format(&self) -> wgpu::TextureFormat {
        self.config.format
    }

    /// Surface size in physical pixels.
    #[must_use]
    pub fn size_px(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    pub fn resize(&mut self, new_size: winit::dpi::PhysicalSize<u32>) {
        if new_size.width == 0 || new_size.height == 0 {
            return;
        }
        self.config.width = new_size.width;
        self.config.height = new_size.height;
        self.surface.configure(&self.device, &self.config);
    }

    pub fn reconfigure(&mut self) {
        self.surface.configure(&self.device, &self.config);
    }

    /// Push the canvas's changed tiles to the GPU.
    ///
    /// Separate from [`Renderer::render`] so the mutable canvas borrow ends before
    /// the frame begins, which keeps the overlay callback free to touch the rest
    /// of the app state.
    pub fn upload_canvas(&mut self, canvas: &mut Canvas) {
        self.canvas_renderer.upload_dirty(&self.queue, canvas);
    }

    /// Execute the editor's pending stroke commands on the GPU.
    ///
    /// Submitted separately from [`Renderer::render`] and before it, so the canvas tiles
    /// are already up to date by the time the frame reads them. An extra submit per frame
    /// is immaterial next to avoiding one per input sample.
    ///
    /// # Ordering
    ///
    /// The frame's ops are grouped into stroke *segments* at each `Begin`. Every segment
    /// needs its own uniform writes (paint, and the per-tile records), and every write in a
    /// submission lands before any of its commands — so a segment boundary is also a
    /// submission boundary. Within a segment there is exactly one write of each, which is
    /// what keeps the common case at one submit.
    pub fn apply_stroke(&mut self, ops: &[StrokeOp], dabs: &[openpaint_core::Dab]) {
        if ops.is_empty() {
            return;
        }

        // Upload every dab once, up front. Per-batch uploads would be clobbered: all of a
        // submission's buffer writes are applied before any of its command buffers
        // execute. See StrokeLayer::upload_dabs.
        self.stroke_layer
            .upload_dabs(&self.device, &self.queue, dabs);
        // Dab clipping reads the page from the stroke layer's uniform, and stamping happens
        // before the frame is drawn -- so the page has to be current *here*, not at render
        // time.
        self.stroke_layer
            .set_page(&self.queue, self.canvas_renderer.page());

        for segment in segments(ops) {
            self.run_segment(segment, dabs);
        }
    }

    /// Execute one stroke segment: at most one `Begin`, its stamping, and its `End`.
    fn run_segment(&mut self, ops: &[StrokeOp], dabs: &[openpaint_core::Dab]) {
        let page = self.canvas_renderer.page();
        let mut encoder = self.new_stroke_encoder();

        if let Some(StrokeOp::Begin {
            color_linear_premul,
            opacity,
        }) = ops.first()
        {
            self.recording.clear();
            self.recording_paint = (*color_linear_premul, *opacity);
            self.stroke_layer
                .set_paint(&self.queue, *color_linear_premul, *opacity);
            self.stroke_layer.begin_stroke();
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
                .stroke_layer
                .prepare_tiles(&self.queue, &mut encoder, span, page);
            for index in 0..tiles {
                for &(start, len) in &ranges {
                    self.stroke_layer
                        .stamp_range(&mut encoder, index, start, len);
                }
            }
        }

        if ops.iter().any(|op| matches!(op, StrokeOp::End)) {
            self.commit_stroke(&mut encoder, page);
        }

        self.queue.submit(std::iter::once(encoder.finish()));
    }

    /// Snapshot what the stroke is about to overwrite, then bake it.
    fn commit_stroke(&mut self, encoder: &mut wgpu::CommandEncoder, page: PageRect) {
        let tiles = self.stroke_layer.tiles_to_bake(page);
        if tiles.is_empty() || self.recording.is_empty() {
            self.stroke_layer
                .bake(&self.queue, encoder, &mut self.canvas_renderer);
            return;
        }

        // Snapshot *before* baking: this is the pre-stroke image undo restores. A
        // GPU-to-GPU copy per tile, so nothing comes back to the CPU on the interactive
        // path.
        let before = self
            .history
            .snapshot_tiles(encoder, &self.canvas_renderer, &tiles);
        self.stroke_layer
            .bake(&self.queue, encoder, &mut self.canvas_renderer);

        match before {
            Some(before) => {
                let (color_linear_premul, opacity) = self.recording_paint;
                self.history.push(Op::Stroke {
                    before,
                    dabs: std::mem::take(&mut self.recording),
                    color_linear_premul,
                    opacity,
                });
            }
            None => {
                // Recording a stroke we cannot fully revert would let undo produce a state
                // that never existed. Refusing is the honest outcome, and the caller
                // surfaces it.
                self.recording.clear();
                self.unrecordable = true;
            }
        }
    }

    /// React to the page being resized.
    ///
    /// The whole operation, now: move the rectangle. No texture is reallocated, no pixel is
    /// copied, and tiles outside the new rectangle are **kept** — which is what makes a crop
    /// non-destructive (DECISIONS §5c) and why there is no snapshot here to take.
    ///
    /// `record` is false when the resize *is* an undo/redo, so reverting a resize does not
    /// push another one.
    pub fn resize_canvas(&mut self, resize: PageResize, record: bool) {
        self.canvas_renderer.set_page(resize.new);
        // Any in-progress stroke belonged to the old geometry.
        self.stroke_layer.abandon();
        self.recording.clear();

        if record {
            self.history.push(Op::Resize { resize });
        }
    }

    /// Discard every tile outside the page, undoably.
    ///
    /// The **only** operation that destroys pixels. Crop deliberately does not: it moves
    /// the page and leaves the tiles behind, so this exists for when the artist wants that
    /// memory back and has decided the content is finished with (DECISIONS §5c).
    ///
    /// Returns how many tiles were released, and whether any had to be kept because there
    /// was no room to record them — dropping a tile it could not record would destroy
    /// pixels with no way back.
    pub fn trim_to_page(&mut self) -> (usize, bool) {
        let outside = self.canvas_renderer.tiles_outside_page();
        if outside.is_empty() {
            return (0, false);
        }
        let mut encoder = self.new_stroke_encoder();
        let mut adopted = Vec::new();
        let mut refused = false;
        for coord in outside {
            match self
                .history
                .adopt_tile(&mut encoder, &self.canvas_renderer, coord)
            {
                Some(slot) => adopted.push((coord, slot)),
                None => refused = true,
            }
        }
        self.queue.submit(std::iter::once(encoder.finish()));

        let released = adopted.len();
        for (coord, _) in &adopted {
            if let Some(slot) = self.canvas_renderer.take_tile(*coord) {
                self.canvas_renderer.release(slot);
            }
        }
        if !adopted.is_empty() {
            self.history.push(Op::Trim { tiles: adopted });
        }
        (released, refused)
    }

    /// Read the canvas back and write it as an sRGB PNG.
    ///
    /// Stalls on the GPU while the readback maps, which is acceptable for an
    /// explicit user action -- the drawing path never reads back.
    pub fn export_png(&self, path: &std::path::Path) -> Result<(), crate::export::ExportError> {
        crate::export::export_tiles_png(&self.device, &self.queue, &self.canvas_renderer, path)
    }

    /// Undo and redo depths, and snapshot bytes held, for display.
    #[must_use]
    pub fn history_status(&self) -> (usize, usize, u64) {
        (
            self.history.undo_depth(),
            self.history.redo_depth(),
            self.history.bytes_held(),
        )
    }

    /// Resident canvas tiles and capacity, for display.
    #[must_use]
    pub fn residency(&self) -> (u32, u32) {
        self.canvas_renderer.residency()
    }

    /// Take the "a stroke could not be recorded" flag, if it was set.
    pub fn take_unrecordable(&mut self) -> bool {
        std::mem::take(&mut self.unrecordable)
    }

    /// Take the "the canvas ran out of tiles" flag, if it was set.
    pub fn take_exhausted(&mut self) -> bool {
        self.stroke_layer.exhausted()
    }

    /// Undo the most recent operation.
    ///
    /// Returns what the caller must reconcile: a geometry change has to be mirrored
    /// onto the page in the editor, since the page's dimensions live there and the
    /// pixels live here.
    pub fn undo(&mut self) -> HistoryChange {
        let Some(op) = self.history.pop_undo() else {
            return HistoryChange::None;
        };
        let change = match &op {
            Op::Stroke { before, .. } => {
                let mut encoder = self.new_stroke_encoder();
                for (coord, was) in before {
                    match was {
                        TileBefore::Content(snapshot) => {
                            // The tile still exists unless a later op removed it, and LIFO
                            // guarantees no later op is still applied.
                            if let Some(dst) = self.canvas_renderer.slot(*coord) {
                                TilePool::copy_layer_from(
                                    &mut encoder,
                                    self.history.pool(),
                                    snapshot,
                                    self.canvas_renderer.pool(),
                                    dst,
                                );
                            }
                        }
                        TileBefore::Absent => {
                            // The stroke created this tile, so undo removes it rather than
                            // leaving a paper tile holding residency for nothing.
                            if let Some(slot) = self.canvas_renderer.take_tile(*coord) {
                                self.canvas_renderer.release(slot);
                            }
                        }
                    }
                }
                self.queue.submit(std::iter::once(encoder.finish()));
                HistoryChange::Pixels
            }
            Op::Resize { resize } => {
                self.resize_canvas(resize.inverted(), false);
                HistoryChange::Geometry { rect: resize.old }
            }
            Op::Trim { tiles } => {
                // Put the discarded tiles back where they were.
                let mut encoder = self.new_stroke_encoder();
                for (coord, snapshot) in tiles {
                    let Some(dst) = self.canvas_renderer.alloc_bare() else {
                        continue;
                    };
                    TilePool::copy_layer_from(
                        &mut encoder,
                        self.history.pool(),
                        snapshot,
                        self.canvas_renderer.pool(),
                        &dst,
                    );
                    if let Some(displaced) = self.canvas_renderer.put_tile(*coord, dst) {
                        self.canvas_renderer.release(displaced);
                    }
                }
                self.queue.submit(std::iter::once(encoder.finish()));
                HistoryChange::Pixels
            }
        };
        self.history.finish_undo(op);
        change
    }

    /// Re-apply the most recent undone operation.
    pub fn redo(&mut self) -> HistoryChange {
        let Some(op) = self.history.pop_redo() else {
            return HistoryChange::None;
        };
        let change = match &op {
            Op::Stroke {
                dabs,
                color_linear_premul,
                opacity,
                ..
            } => {
                // Replayed rather than restored from an after-image: half the memory, and
                // re-rasterizing is the cheap direction now that dabs are stamped on the
                // GPU.
                let page = self.canvas_renderer.page();
                self.stroke_layer.set_page(&self.queue, page);
                self.stroke_layer
                    .set_paint(&self.queue, *color_linear_premul, *opacity);
                self.stroke_layer
                    .upload_dabs(&self.device, &self.queue, dabs);

                let mut encoder = self.new_stroke_encoder();
                self.stroke_layer.begin_stroke();
                let tiles = self
                    .stroke_layer
                    .prepare_tiles(&self.queue, &mut encoder, dabs, page);
                for index in 0..tiles {
                    self.stroke_layer
                        .stamp_range(&mut encoder, index, 0, dabs.len());
                }
                self.stroke_layer
                    .bake(&self.queue, &mut encoder, &mut self.canvas_renderer);
                self.queue.submit(std::iter::once(encoder.finish()));
                HistoryChange::Pixels
            }
            Op::Resize { resize } => {
                self.resize_canvas(*resize, false);
                HistoryChange::Geometry { rect: resize.new }
            }
            Op::Trim { tiles } => {
                for (coord, _) in tiles {
                    if let Some(slot) = self.canvas_renderer.take_tile(*coord) {
                        self.canvas_renderer.release(slot);
                    }
                }
                HistoryChange::Pixels
            }
        };
        self.history.finish_redo(op);
        change
    }

    fn new_stroke_encoder(&self) -> wgpu::CommandEncoder {
        self.device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("stroke-encoder"),
            })
    }

    /// Draw one frame: clear the backdrop, draw the canvas, then the overlay.
    ///
    /// Takes an already-computed transform rather than the canvas: the renderer has no
    /// need for the document (the pixels are already in its tiles), and not borrowing it
    /// leaves the caller free to hand the overlay mutable access to editor state.
    pub fn render(
        &mut self,
        xform: PageToNdc,
        overlay: impl FnOnce(Overlay<'_>),
    ) -> Result<(), wgpu::SurfaceError> {
        let page = self.canvas_renderer.page();
        self.canvas_renderer
            .prepare(&self.device, &self.queue, xform);
        self.stroke_layer.set_frame(&self.queue, xform, page);
        if self.stroke_layer.has_paint() {
            self.stroke_layer
                .prepare_preview(&self.device, &self.queue, page);
        }

        let frame = self.surface.get_current_texture()?;
        let target = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame-encoder"),
            });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("canvas-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // Neutral gray backdrop framing the canvas sheet.
                        // LINEAR values: the surface is an *Srgb format, so wgpu
                        // encodes on write. These are sRGB 0.16/0.16/0.17 taken
                        // through the transfer function -- passing the sRGB
                        // numbers directly would render a washed-out mid-grey.
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.021_9,
                            g: 0.021_9,
                            b: 0.024_7,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            self.canvas_renderer.draw(&mut pass);
            // The in-progress stroke is not in the canvas texture yet, so it is
            // composited on top for the preview. It uses the same placement, so it
            // pans, zooms, and rotates with the canvas.
            if self.stroke_layer.has_paint() {
                self.stroke_layer.draw_preview(&mut pass);
            }
        }

        overlay(Overlay {
            device: &self.device,
            queue: &self.queue,
            encoder: &mut encoder,
            target: &target,
            size_px: [self.config.width, self.config.height],
        });

        self.queue.submit(std::iter::once(encoder.finish()));
        frame.present();
        Ok(())
    }

    #[must_use]
    pub fn window(&self) -> &Arc<Window> {
        &self.window
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
}
