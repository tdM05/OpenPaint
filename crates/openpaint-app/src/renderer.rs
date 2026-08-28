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

use winit::window::Window;

use crate::canvas_renderer::{CanvasRenderer, CANVAS_FORMAT};
use crate::editor::StrokeOp;
use crate::history::{History, Op, TileBefore};
use crate::stroke_exec::StrokeExec;
use crate::stroke_layer::StrokeLayer;
use crate::tile_pool::{Slot, TilePool};
use crate::tile_store::{LayerId, TileKey};
use crate::view::PageToNdc;
use openpaint_core::{Layer, PageRect, PageResize};

/// What an undo or redo did, and therefore what the caller must reconcile.
///
/// Geometry changes cannot be handled entirely in here: the page's dimensions live in
/// the editor while its pixels live on the GPU, so the shell has to apply both halves.
#[derive(Clone, Debug, PartialEq)]
pub enum HistoryChange {
    /// Nothing to undo or redo.
    None,
    /// Pixels changed; just redraw.
    Pixels,
    /// The page's rectangle changed; the editor's page must be moved to match.
    Geometry { rect: openpaint_core::PageRect },
    /// A deleted layer came back, and the document must put it at `index` again.
    LayerRestored { index: usize, layer: Layer },
    /// A restored layer was deleted again.
    LayerDeleted { index: usize },
    /// A deleted page came back, and the document must put it at `index` again.
    PageRestored {
        index: usize,
        page: openpaint_core::Page,
    },
    /// A restored page was deleted again.
    PageDeleted { index: usize },
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
    /// Paint of the stroke being recorded, captured at Begin: colour, opacity ceiling, and
    /// whether it erases.
    recording_paint: ([f32; 4], f32, bool),
    /// Set when a stroke could not be recorded for undo because the snapshot pool was
    /// full even after evicting everything.
    unrecordable: bool,
    /// Set when the last frame's visible tiles did not all fit in the pool.
    pressured: bool,
    window: Arc<Window>,
}

impl Renderer {
    /// Create the GPU context for a window. Blocks on async setup via pollster.
    pub fn new(window: Arc<Window>, page: PageRect) -> Result<Self, String> {
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
            // Fifo, deliberately: a paint app is not a game, and tearing across a canvas you are
            // staring at is worse than a frame of latency. It also idles instead of spinning,
            // which matters on the battery-powered §2 target.
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            // One, not the usual two. Frame latency is how many frames the driver will accept
            // before making us wait, so it is *bought* latency: at two, a stroke can sit finished
            // in the queue for an extra display refresh before anyone sees it. Two exists to keep
            // GPUs fed when frames are expensive and irregular; ours cost ~1 ms of work against a
            // 16.7 ms refresh, so there is nothing to smooth over and the second frame of buffer
            // is pure delay.
            //
            // **Kept, but unproven.** Measured stroke latency was 16.6 ms mean / 19.1 peak at two
            // and 16.4 / 19.3 at one — no change. That is expected rather than reassuring: our
            // measurement stops when `present` returns, and the presentation queue this setting
            // governs is entirely downstream of that, so the experiment could not have shown a
            // win even if there were one. It stays at one because frames are cheap enough that the
            // extra buffer cannot help, and DECISIONS §4.1 says which way to lean when a trade is
            // unresolved. Reading the tablet's own clock (Q10a.3) is what would settle it.
            desired_maximum_frame_latency: 1,
        };
        surface.configure(&device, &config);

        // Residency is sized from what the adapter looks like, because wgpu offers no way
        // to ask how much graphics memory there actually is. See `tile_store::budget_for`.
        let budget = crate::tile_store::budget_for(info.device_type);
        // The stroke layer first: the compositor reads its accumulation to show the preview,
        // so it needs that texture at construction (see `canvas_renderer`).
        let stroke_layer = StrokeLayer::new(&device, CANVAS_FORMAT);
        let canvas_renderer = CanvasRenderer::new(&device, format, page, budget, &stroke_layer);
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
            recording_paint: ([0.0; 4], 1.0, false),
            unrecordable: false,
            pressured: false,
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

    /// Execute the editor's pending stroke commands on the GPU.
    ///
    /// Submitted separately from [`Renderer::render`] and before it, so the canvas tiles are
    /// already up to date by the time the frame reads them. An extra submit per frame is
    /// immaterial next to avoiding one per input sample.
    ///
    /// The work itself lives in [`crate::stroke_exec`], which needs no surface and is
    /// therefore testable — see the note there.
    pub fn apply_stroke(&mut self, ops: &[StrokeOp], dabs: &[openpaint_core::Dab], layer: LayerId) {
        let mut exec = StrokeExec {
            layer,
            device: &self.device,
            queue: &self.queue,
            canvas: &mut self.canvas_renderer,
            stroke: &mut self.stroke_layer,
            history: &mut self.history,
            recording: &mut self.recording,
            recording_paint: &mut self.recording_paint,
        };
        self.unrecordable |= exec.run(ops, dabs);
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

    /// Point the canvas at a different page rectangle, without recording anything.
    ///
    /// For switching pages and for loading, where the rectangle change *is* the navigation
    /// rather than an edit. A resize goes through `resize_canvas` so it lands in history.
    pub fn set_page(&mut self, page: PageRect) {
        self.canvas_renderer.set_page(page);
        self.stroke_layer.abandon();
        self.recording.clear();
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
        for key in outside {
            match self
                .history
                .adopt_tile(&mut encoder, &self.canvas_renderer, key)
            {
                Some(slot) => adopted.push((key, slot)),
                None => refused = true,
            }
        }
        self.queue.submit(std::iter::once(encoder.finish()));

        let released = adopted.len();
        for (key, _) in &adopted {
            if let Some(slot) = self.canvas_renderer.take_tile(*key) {
                self.canvas_renderer.release(slot);
            }
        }
        if !adopted.is_empty() {
            self.history.push(Op::Trim { tiles: adopted });
        }
        (released, refused)
    }

    /// Move a layer's tiles into history and drop them, so the layer can be deleted undoably.
    ///
    /// Returns `None` if there was no room to record them all, in which case the caller must
    /// **not** delete the layer: destroying pixels with no way back is the one thing this
    /// project does not do (DECISIONS §5c).
    pub fn adopt_layer(&mut self, layer: LayerId) -> Option<Vec<(TileKey, Slot)>> {
        let coords: Vec<openpaint_core::tile::TileCoord> =
            self.canvas_renderer.layer_tiles(layer).collect();
        let mut encoder = self.new_stroke_encoder();
        let mut adopted = Vec::with_capacity(coords.len());
        for coord in coords {
            let key = TileKey::new(layer, coord);
            match self
                .history
                .adopt_tile(&mut encoder, &self.canvas_renderer, key)
            {
                Some(slot) => adopted.push((key, slot)),
                None => {
                    // Give back what this attempt took, so a refusal costs nothing.
                    self.queue.submit(std::iter::once(encoder.finish()));
                    for (_, slot) in adopted {
                        self.history.release_slot(slot);
                    }
                    return None;
                }
            }
        }
        self.queue.submit(std::iter::once(encoder.finish()));
        self.canvas_renderer.discard_layer(layer);
        Some(adopted)
    }

    /// Move every tile of every layer of a page into history, so the page can be deleted
    /// undoably. Returns `None` if there was no room to record them all.
    pub fn adopt_page(&mut self, page: &openpaint_core::Page) -> Option<Vec<(TileKey, Slot)>> {
        let mut all = Vec::new();
        for layer in page.layers() {
            match self.adopt_layer(LayerId(layer.id())) {
                Some(mut tiles) => all.append(&mut tiles),
                None => {
                    // Give back what earlier layers contributed, so a refusal costs nothing and
                    // leaves the page intact.
                    for (_, slot) in all {
                        self.history.release_slot(slot);
                    }
                    return None;
                }
            }
        }
        Some(all)
    }

    /// Record a completed page deletion, whose tiles [`Renderer::adopt_page`] already took.
    pub fn record_page_deletion(
        &mut self,
        index: usize,
        page: openpaint_core::Page,
        tiles: Vec<(TileKey, Slot)>,
    ) {
        self.history.push(Op::DeletePage { index, page, tiles });
    }

    /// Record a completed layer deletion, whose tiles [`Renderer::adopt_layer`] already took.
    pub fn record_layer_deletion(
        &mut self,
        index: usize,
        layer: Layer,
        tiles: Vec<(TileKey, Slot)>,
    ) {
        self.history.push(Op::DeleteLayer {
            index,
            layer,
            tiles,
        });
    }

    /// Save the document and every tile it holds to `path`.
    ///
    /// Stalls to read the resident tiles back, which is fine for an explicit save; the drawing
    /// path never reads back. Undo history is deliberately not saved -- see `openpaint_file`.
    /// The composited colour at a page pixel, as the artist sees it.
    ///
    /// Delegates to [`CanvasRenderer::sample_page_pixel`], which owns the tiles and is testable
    /// without a surface.
    pub fn sample_page_pixel(&mut self, x: i32, y: i32, layers: &[Layer]) -> [f32; 4] {
        self.canvas_renderer
            .sample_page_pixel(&self.device, &self.queue, x, y, layers)
    }

    pub fn save_document(
        &mut self,
        document: &openpaint_core::Document,
        path: &std::path::Path,
        meta: &[(&str, &str)],
    ) -> Result<usize, openpaint_file::Error> {
        // Page index per layer id, so a tile can be filed under the page its layer belongs to.
        let mut page_of_layer = std::collections::HashMap::new();
        for index in 0..document.page_count() {
            if let Some(page) = document.page(index) {
                for layer in page.layers() {
                    page_of_layer.insert(layer.id(), index);
                }
            }
        }

        let tiles = self.canvas_renderer.snapshot_all(&self.device, &self.queue);
        let count = tiles.len();
        let refs = tiles.into_iter().filter_map(|(key, tile)| {
            // A tile whose layer no longer exists belongs to nothing and is not saved. It can
            // only be one a deleted layer left in history, which a save does not preserve
            // anyway.
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
        openpaint_file::save(path, document, refs, meta)?;
        Ok(count)
    }

    /// Adopt the tiles of a freshly loaded document, discarding whatever was here.
    ///
    /// The caller replaces the `Document` itself; this takes the pixels. History is cleared with
    /// them, because an undo stack that outlived its document would restore tiles into layers
    /// that no longer exist.
    pub fn load_document(
        &mut self,
        page: PageRect,
        loaded_tiles: Vec<(openpaint_file::TileRef, openpaint_core::tile::Tile)>,
    ) {
        self.history = History::new(&self.device);
        self.stroke_layer.abandon();
        self.recording.clear();
        self.canvas_renderer.set_page(page);
        self.canvas_renderer.load_tiles(
            loaded_tiles
                .into_iter()
                .map(|(r, t)| (TileKey::new(LayerId(r.layer_id), r.coord), t)),
        );
    }

    /// Read the canvas back and write it as an sRGB PNG.
    ///
    /// Stalls on the GPU while the readback maps, which is acceptable for an
    /// explicit user action -- the drawing path never reads back.
    pub fn export_png(
        &self,
        layers: &[Layer],
        path: &std::path::Path,
    ) -> Result<(), crate::export::ExportError> {
        crate::export::export_tiles_png(
            &self.device,
            &self.queue,
            &self.canvas_renderer,
            layers,
            path,
        )
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

    /// Whether the last frame could not make every visible tile resident.
    #[must_use]
    pub fn pressured(&self) -> bool {
        self.pressured
    }

    /// Tiles held on the CPU, and (readbacks, re-uploads) so far.
    #[must_use]
    pub fn spill_status(&self) -> (usize, (u64, u64)) {
        (
            self.canvas_renderer.spilled_count(),
            self.canvas_renderer.traffic(),
        )
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
            Op::Stroke { layer, before, .. } => {
                let mut encoder = self.new_stroke_encoder();
                for (coord, was) in before {
                    match was {
                        TileBefore::Content(snapshot) => {
                            // The tile still exists unless a later op removed it, and LIFO
                            // guarantees no later op is still applied.
                            if let Some(dst) = self.canvas_renderer.slot(*layer, *coord) {
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
                            // leaving an empty tile holding residency for nothing.
                            if let Some(slot) =
                                self.canvas_renderer.take_tile(TileKey::new(*layer, *coord))
                            {
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
                self.restore_tiles(tiles);
                HistoryChange::Pixels
            }
            Op::DeleteLayer {
                index,
                layer,
                tiles,
            } => {
                self.restore_tiles(tiles);
                HistoryChange::LayerRestored {
                    index: *index,
                    layer: layer.clone(),
                }
            }
            Op::DeletePage { index, page, tiles } => {
                self.restore_tiles(tiles);
                HistoryChange::PageRestored {
                    index: *index,
                    page: page.clone(),
                }
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
                layer,
                dabs,
                color_linear_premul,
                opacity,
                erase,
                ..
            } => {
                // Replayed rather than restored from an after-image: half the memory, and
                // re-rasterizing is the cheap direction now that dabs are stamped on the
                // GPU.
                let page = self.canvas_renderer.page();
                self.stroke_layer.set_page(&self.queue, page);
                self.stroke_layer
                    .set_paint(&self.queue, *color_linear_premul, *opacity, *erase);
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
                self.stroke_layer.bake(
                    &self.device,
                    &self.queue,
                    &mut encoder,
                    &mut self.canvas_renderer,
                    *layer,
                );
                self.queue.submit(std::iter::once(encoder.finish()));
                HistoryChange::Pixels
            }
            Op::Resize { resize } => {
                self.resize_canvas(*resize, false);
                HistoryChange::Geometry { rect: resize.new }
            }
            Op::Trim { tiles } => {
                for (key, _) in tiles {
                    if let Some(slot) = self.canvas_renderer.take_tile(*key) {
                        self.canvas_renderer.release(slot);
                    }
                }
                HistoryChange::Pixels
            }
            Op::DeleteLayer { index, layer, .. } => {
                self.canvas_renderer.discard_layer(LayerId(layer.id()));
                HistoryChange::LayerDeleted { index: *index }
            }
            Op::DeletePage { index, page, .. } => {
                for layer in page.layers() {
                    self.canvas_renderer.discard_layer(LayerId(layer.id()));
                }
                HistoryChange::PageDeleted { index: *index }
            }
        };
        self.history.finish_redo(op);
        change
    }

    /// Copy snapshot tiles back into the canvas, allocating fresh pool layers for them.
    fn restore_tiles(&mut self, tiles: &[(TileKey, Slot)]) {
        let mut encoder = self.new_stroke_encoder();
        for (key, snapshot) in tiles {
            let Some(dst) = self.canvas_renderer.alloc_bare(&self.device, &self.queue) else {
                continue;
            };
            TilePool::copy_layer_from(
                &mut encoder,
                self.history.pool(),
                snapshot,
                self.canvas_renderer.pool(),
                &dst,
            );
            if let Some(displaced) = self.canvas_renderer.put_tile(*key, dst) {
                self.canvas_renderer.release(displaced);
            }
        }
        self.queue.submit(std::iter::once(encoder.finish()));
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
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        xform: PageToNdc,
        visible: PageRect,
        layers: &[Layer],
        active: usize,
        overlay: impl FnOnce(Overlay<'_>),
    ) -> Result<(), wgpu::SurfaceError> {
        let page = self.canvas_renderer.page();

        // Restores and fresh-tile clears are recorded here and submitted before the frame, so
        // the pass that samples them runs afterwards.
        let mut prep = self.new_stroke_encoder();
        self.canvas_renderer.begin_frame();
        self.pressured = self.canvas_renderer.prepare(
            &self.device,
            &self.queue,
            &mut prep,
            xform,
            visible,
            layers,
            active,
            Some(&self.stroke_layer),
        );
        self.queue.submit(std::iter::once(prep.finish()));
        self.stroke_layer.set_frame(&self.queue, xform, page);

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
            // One call: the compositor walks the layer stack and injects the in-progress
            // stroke into the active layer as it goes, so there is no separate preview pass.
            self.canvas_renderer.draw(&mut pass);
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
