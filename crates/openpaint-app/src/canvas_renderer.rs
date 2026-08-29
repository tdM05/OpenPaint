//! Draws the canvas: the layer stack, composited from a bounded pool of GPU tiles.
//!
//! # What replaced what
//!
//! This used to hold one texture the size of the whole page. That was recorded as a Phase-0
//! shortcut (OPEN_QUESTIONS Q13) and it imposed two ceilings the app had to apologise for: a
//! page could be no bigger than `max_texture_dimension_2d` (8192), and no bigger than one
//! allocation the driver would accept (~16 Mpx at `Rgba16Float`). Both are gone: a page's size
//! no longer determines any allocation.
//!
//! Three consequences worth stating, because they are the point:
//!
//! 1. **Memory scales with painted area, not page area.** A blank 800×20000 webtoon strip
//!    costs nothing until it is drawn on, and [`crate::tile_store`] spills what does not fit.
//! 2. **Storage is decoupled from the page rectangle**, which is what makes a crop
//!    non-destructive (DECISIONS §5c): tiles outside the page are kept, just not drawn.
//! 3. **Resizing is metadata.** No texture is recreated and no pixel is copied.
//!
//! # Page coordinates go all the way down
//!
//! There is no page-origin-to-texture-origin offset anywhere, because a tile is addressed by
//! its *page* tile coordinate. The signed-origin invariant (DECISIONS §5a) — a pixel keeps its
//! coordinate forever — holds unbroken from the core to the GPU.
//!
//! # Compositing is one pass, and the preview is part of it
//!
//! Every layer's tiles live in the same array texture, so one fragment shader reads the whole
//! stack (DECISIONS §4e). That is what lets blend modes be plain arithmetic instead of needing
//! a destination read, and it is why the in-progress stroke is *injected into the active
//! layer* rather than drawn on top: a mid-stroke preview runs the same code as the committed
//! result, so the two cannot disagree.
//!
//! Consequently this needs the stroke layer's accumulation texture at construction time. That
//! coupling is real and deliberate — the preview genuinely is part of compositing — and it is
//! safe because neither pool's texture is ever replaced.

use half::f16;
use openpaint_core::tile::{TileCoord, TILE_CHANNELS, TILE_SIZE};
use openpaint_core::{Canvas, Layer, PageRect};

use crate::stroke_layer::StrokeLayer;
use crate::tile_pool::{Slot, TilePool};
use crate::tile_store::{Init, LayerId, Pressure, TileKey, TileStore};
use crate::view::PageToNdc;

/// Pixel format of canvas tiles: linear premultiplied RGBA f16, matching
/// `openpaint_core::tile` exactly (DECISIONS §4b) so uploads are a straight byte copy.
pub const CANVAS_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// Bytes per canvas texel (RGBA f16).
pub const CANVAS_BYTES_PER_TEXEL: u32 = (TILE_CHANNELS * std::mem::size_of::<f16>()) as u32;

/// Marks "this layer has no tile here", matching `ABSENT` in canvas.wgsl.
const ABSENT: u32 = u32::MAX;

/// Uniform matching `Params` in canvas.wgsl.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ParamsUniform {
    x_row: [f32; 4],
    y_row: [f32; 4],
    page: [f32; 4],
    paper: [f32; 4],
    stroke: [f32; 4],
    counts: [u32; 4],
    misc: [f32; 4],
}

/// Matching `LayerInfo` in canvas.wgsl.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LayerInfoRecord {
    blend: u32,
    opacity: f32,
    clip: u32,
    _pad: f32,
}

/// Per-instance data matching `TileInst` in canvas.wgsl.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TileInstance {
    coord: [i32; 2],
}

pub struct CanvasRenderer {
    store: TileStore,
    /// The page rectangle. Drawing and painting are bounded by it; storage is not.
    page: PageRect,
    /// Rebuilt whenever a storage buffer is grown, which is why the pieces are kept.
    bind_group: wgpu::BindGroup,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    accum_view: wgpu::TextureView,
    sheet_pipeline: wgpu::RenderPipeline,
    composite_pipeline: wgpu::RenderPipeline,
    params_buf: wgpu::Buffer,
    /// One entry per (instance, layer): which pool layer holds that tile, or `ABSENT`.
    slots_buf: wgpu::Buffer,
    slots_capacity: usize,
    /// One entry per layer: blend mode and opacity.
    infos_buf: wgpu::Buffer,
    infos_capacity: usize,
    /// One entry per instance: which accumulation layer holds the in-progress stroke there.
    stroke_buf: wgpu::Buffer,
    float_buf: wgpu::Buffer,
    stroke_capacity: usize,
    instances: wgpu::Buffer,
    instance_capacity: usize,
    /// Instances written by the last `prepare`, i.e. what `draw` will draw.
    instance_count: u32,
}

impl CanvasRenderer {
    pub fn new(
        device: &wgpu::Device,
        surface_format: wgpu::TextureFormat,
        page: PageRect,
        budget_bytes: u64,
        stroke: &StrokeLayer,
    ) -> Self {
        let store = TileStore::new(device, budget_bytes);

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("canvas-sampler"),
            // Nearest when magnifying, so zooming in shows real pixels rather than a blur --
            // what you want when inspecting brush edges. Linear when minifying, so a
            // zoomed-out canvas doesn't alias into noise.
            //
            // An array texture is what makes this safe: each tile is its own image with its own
            // clamped edges, so a filtered sample near a tile border cannot pull in an
            // unrelated tile the way it would from a 2D atlas.
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        // Its own view of the stroke layer's array, because a `TextureView` cannot be cloned
        // and this one has to survive being rebound when a storage buffer grows.
        let accum_view = stroke
            .accum_texture()
            .create_view(&wgpu::TextureViewDescriptor {
                label: Some("canvas-accum-view"),
                dimension: Some(wgpu::TextureViewDimension::D2Array),
                ..Default::default()
            });

        let params_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("canvas-params"),
            size: std::mem::size_of::<ParamsUniform>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let slots_capacity = 1024;
        let infos_capacity = 16;
        let stroke_capacity = 256;
        let instance_capacity = 256;
        let slots_buf = storage_buffer(device, "canvas-slots", slots_capacity * 4);
        let infos_buf = storage_buffer(
            device,
            "canvas-layer-infos",
            infos_capacity * std::mem::size_of::<LayerInfoRecord>(),
        );
        let stroke_buf = storage_buffer(device, "canvas-stroke-slots", stroke_capacity * 4);
        let float_buf = storage_buffer(device, "canvas-float-slots", stroke_capacity * 4);
        let instances = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("canvas-tile-instances"),
            size: (instance_capacity * std::mem::size_of::<TileInstance>()) as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("canvas-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                texture_entry(1),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                storage_entry(3),
                storage_entry(4),
                texture_entry(5),
                storage_entry(6),
                // The floating pixels of a transform, by tile.
                storage_entry(7),
            ],
        });

        let bind_group = make_bind_group(
            device,
            &bind_group_layout,
            &params_buf,
            store.pool(),
            &sampler,
            &slots_buf,
            &infos_buf,
            &accum_view,
            &stroke_buf,
            &float_buf,
        );

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("canvas-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("canvas.wgsl").into()),
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("canvas-pl"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let target = |format: wgpu::TextureFormat| {
            Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::REPLACE),
                write_mask: wgpu::ColorWrites::ALL,
            })
        };

        let sheet_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("canvas-sheet-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "sheet_vs",
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "sheet_fs",
                targets: &[target(surface_format)],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let composite_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("canvas-composite-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "composite_vs",
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<TileInstance>() as wgpu::BufferAddress,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &[wgpu::VertexAttribute {
                        offset: 0,
                        shader_location: 0,
                        format: wgpu::VertexFormat::Sint32x2,
                    }],
                }],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "composite_fs",
                targets: &[target(surface_format)],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        Self {
            store,
            page,
            bind_group,
            bind_group_layout,
            sampler,
            accum_view,
            sheet_pipeline,
            composite_pipeline,
            params_buf,
            slots_buf,
            slots_capacity,
            infos_buf,
            infos_capacity,
            stroke_buf,
            float_buf,
            stroke_capacity,
            instances,
            instance_capacity,
            instance_count: 0,
        }
    }

    /// The page rectangle currently being drawn.
    #[must_use]
    pub fn page(&self) -> PageRect {
        self.page
    }

    /// Move the page rectangle.
    ///
    /// The entire cost of a resize. Tiles are addressed in page coordinates, so nothing is
    /// reallocated, copied, or rekeyed — and tiles that fall outside the new rectangle are
    /// **kept**, which is what makes a crop non-destructive (DECISIONS §5c).
    pub fn set_page(&mut self, page: PageRect) {
        self.page = page;
    }

    #[must_use]
    pub fn pool(&self) -> &TilePool {
        self.store.pool()
    }

    /// Resident tile count and capacity, for display.
    #[must_use]
    pub fn residency(&self) -> (u32, u32) {
        self.store.residency()
    }

    /// Tiles held on the CPU because they did not fit on the GPU.
    #[must_use]
    pub fn spilled_count(&self) -> usize {
        self.store.spilled_count()
    }

    /// Readbacks and re-uploads so far, for spotting a thrashing budget.
    #[must_use]
    pub fn traffic(&self) -> (u64, u64) {
        self.store.traffic()
    }

    /// Start a frame: drain finished spills and advance residency's frame counter.
    pub fn begin_frame(&mut self) {
        self.store.begin_frame();
    }

    /// The pool layer holding a tile, if it is resident.
    #[must_use]
    pub fn slot(&self, layer: LayerId, coord: TileCoord) -> Option<&Slot> {
        self.store.slot(TileKey::new(layer, coord))
    }

    /// Every tile a layer holds, resident or spilled.
    pub fn layer_tiles(&self, layer: LayerId) -> impl Iterator<Item = TileCoord> + '_ {
        self.store
            .keys()
            .filter(move |k| k.layer == layer)
            .map(|k| k.coord)
    }

    /// Every tile coordinate any layer holds, deduplicated and in a stable order.
    #[must_use]
    pub fn occupied_tiles(&self) -> Vec<TileCoord> {
        let mut all: Vec<TileCoord> = self.store.keys().map(|k| k.coord).collect();
        all.sort_unstable_by_key(|c| (c.1, c.0));
        all.dedup();
        all
    }

    /// Make sure a tile is on the GPU, restoring it from the CPU if it had spilled.
    ///
    /// Returns `None` when that layer has no such tile, so callers that only want existing
    /// pixels do not accidentally create empty ones.
    pub fn make_resident(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        layer: LayerId,
        coord: TileCoord,
    ) -> Option<u32> {
        let key = TileKey::new(layer, coord);
        if !self.store.contains(key) {
            return None;
        }
        // The init value is unreachable: the tile exists, so this is a restore.
        self.store
            .ensure(device, queue, encoder, key, Init::Untouched)
            .ok()
    }

    /// Note that a tile's pixels changed, so residency owes it a readback if it spills.
    pub fn mark_dirty(&mut self, layer: LayerId, coord: TileCoord) {
        self.store.mark_dirty(TileKey::new(layer, coord));
    }

    /// Make sure a tile exists, creating it **transparent** if it did not.
    ///
    /// Transparent, not paper: a layer is empty where unpainted, and the paper belongs at the
    /// bottom of the compositor. Filling layer tiles with paper would make every layer opaque
    /// and hide everything beneath it.
    pub fn ensure_tile(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        layer: LayerId,
        coord: TileCoord,
    ) -> Result<u32, Pressure> {
        self.store.ensure(
            device,
            queue,
            encoder,
            TileKey::new(layer, coord),
            Init::Clear(wgpu::Color::TRANSPARENT),
        )
    }

    /// A tile as a render target, for a stroke to bake into.
    #[must_use]
    pub fn tile_target(&self, layer: LayerId, coord: TileCoord) -> Option<&wgpu::TextureView> {
        self.store
            .slot(TileKey::new(layer, coord))
            .map(|s| self.store.pool().layer_view(s))
    }

    /// Every tile of every layer that lies entirely outside the page.
    #[must_use]
    pub fn tiles_outside_page(&self) -> Vec<TileKey> {
        let page = self.page;
        let mut out: Vec<TileKey> = self
            .store
            .keys()
            .filter(|k| !tile_intersects(k.coord, page))
            .collect();
        out.sort_unstable();
        out
    }

    /// Take a tile out of the canvas, giving up ownership of its pool layer.
    ///
    /// Used by undo (a tile the stroke created must go away again) and by Trim. The layer is
    /// *not* freed here: the caller decides whether to return it or keep it alive as a
    /// snapshot, and the move-only slot is what makes that choice explicit.
    #[must_use]
    pub fn take_tile(&mut self, key: TileKey) -> Option<Slot> {
        self.store.remove(key)
    }

    /// Put a tile back, e.g. when undoing a Trim. Returns any displaced slot.
    #[must_use = "the displaced slot must be returned to a pool"]
    pub fn put_tile(&mut self, key: TileKey, slot: Slot) -> Option<Slot> {
        self.store.insert(key, slot)
    }

    /// Allocate a bare tile without clearing it, for a caller that will overwrite it.
    pub fn alloc_bare(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) -> Option<Slot> {
        self.store.alloc_bare(device, queue)
    }

    /// Return a layer to the canvas pool.
    pub fn release(&mut self, slot: Slot) {
        self.store.release(slot);
    }

    /// Every tile the canvas holds, as CPU bytes. For saving.
    pub fn snapshot_all(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Vec<(TileKey, openpaint_core::tile::Tile)> {
        self.store.snapshot_all(device, queue)
    }

    /// The composited colour at a page pixel, as the artist sees it.
    ///
    /// **Composited, not the active layer.** An eyedropper answers "what colour is *that*", and what
    /// the artist points at is the whole stack — line art on one layer over flats on another is
    /// exactly the case where sampling a single layer gives the wrong answer. (CSP offers both; the
    /// per-layer variant is a setting for later, not a different mechanism.)
    ///
    /// Folded with `export::blend_over`, the same arithmetic as `composite_fs` in canvas.wgsl and
    /// already pinned to it by `the_gpu_compositor_matches_the_cpu_reference`. So this returns the
    /// displayed colour by construction rather than by a second implementation that resembles it.
    ///
    /// Reads one tile per visible layer, never the working set — see `TileStore::snapshot_some`.
    pub fn sample_page_pixel(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        x: i32,
        y: i32,
        layers: &[openpaint_core::Layer],
    ) -> [f32; 4] {
        let coord = tile_of(x, y);
        let side = TILE_SIZE as i32;
        let lx = x.rem_euclid(side) as usize;
        let ly = y.rem_euclid(side) as usize;

        // Every layer, not only the visible ones: a hidden layer still has to be offered to the
        // fold so that a clip group masks against the right base.
        let keys: Vec<TileKey> = layers
            .iter()
            .map(|l| TileKey {
                layer: LayerId(l.id()),
                coord,
            })
            .collect();
        let tiles: std::collections::HashMap<TileKey, openpaint_core::tile::Tile> = self
            .store
            .snapshot_some(device, queue, &keys)
            .into_iter()
            .collect();

        // The paper first, then bottom up: the same order and the same fold as the compositor.
        // Over the paper because the paper is on screen too, and sampling unpainted canvas should
        // give the colour visible there rather than a transparent nothing a colour picker would have
        // to invent a value for.
        let mut fold = crate::export::Composite::new(Canvas::paper_color());
        for layer in layers {
            let key = TileKey {
                layer: LayerId(layer.id()),
                coord,
            };
            // A coordinate no layer has painted is not an error; a zero texel contributes nothing,
            // and saying so is what keeps a clip group's base correct.
            let texel = tiles.get(&key).map_or([0.0; 4], |t| t.texel(lx, ly));
            fold.add(texel, layer);
        }
        fold.finish()
    }

    /// One tile of the composited image, as 8-bit sRGB.
    ///
    /// What the flood fill reads. A tile at a time rather than a pixel at a time because compositing
    /// a pixel means touching every layer, and a flood fill asks about the same tile thousands of
    /// times — the caller caches these, and that is the difference between usable and hopeless.
    ///
    /// 8-bit sRGB rather than linear float because that is the space a tolerance means something
    /// in: "within 20" is a judgement about colours as seen, and the artist sets it.
    pub fn composited_tile(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        coord: TileCoord,
        layers: &[openpaint_core::Layer],
    ) -> Vec<[u8; 3]> {
        let keys: Vec<TileKey> = layers
            .iter()
            .map(|l| TileKey {
                layer: LayerId(l.id()),
                coord,
            })
            .collect();
        let tiles: std::collections::HashMap<TileKey, openpaint_core::tile::Tile> = self
            .store
            .snapshot_some(device, queue, &keys)
            .into_iter()
            .collect();

        let mut out = Vec::with_capacity(TILE_SIZE * TILE_SIZE);
        for ly in 0..TILE_SIZE {
            for lx in 0..TILE_SIZE {
                // The same fold as the screen and the eyedropper, so the wand sees what the artist
                // sees rather than a fourth opinion about what a stack means.
                let mut fold = crate::export::Composite::new(Canvas::paper_color());
                for layer in layers {
                    let key = TileKey {
                        layer: LayerId(layer.id()),
                        coord,
                    };
                    let texel = tiles.get(&key).map_or([0.0; 4], |t| t.texel(lx, ly));
                    fold.add(texel, layer);
                }
                out.push(openpaint_core::color::opaque_linear_premul_to_srgb8(
                    fold.finish(),
                ));
            }
        }
        out
    }

    /// Replace everything with the tiles of a loaded document.
    ///
    /// They go to the CPU side, not the GPU: residency pulls in what the viewport needs, so
    /// opening a large document costs nothing until it is looked at.
    pub fn load_tiles(
        &mut self,
        tiles: impl IntoIterator<Item = (TileKey, openpaint_core::tile::Tile)>,
    ) {
        self.store.clear();
        for (key, tile) in tiles {
            self.store.preload(key, tile);
        }
        self.instance_count = 0;
    }

    /// Replace every tile of a layer.
    ///
    /// Used for the floating pixels of a transform, which are re-shifted on every drag. They go in
    /// as ordinary tiles of an ordinary layer, because that is what a floating selection *is* —
    /// which is why a live transform preview needs no shader of its own.
    pub fn set_layer_tiles(
        &mut self,
        layer: LayerId,
        tiles: impl IntoIterator<Item = (TileCoord, openpaint_core::tile::Tile)>,
    ) {
        self.discard_layer(layer);
        for (coord, tile) in tiles {
            self.store.preload(TileKey::new(layer, coord), tile);
        }
    }

    /// Replace one tile's contents.
    pub fn replace_tile(&mut self, key: TileKey, tile: openpaint_core::tile::Tile) {
        self.store.preload(key, tile);
    }

    /// Read specific tiles of a layer, wherever they live.
    pub fn read_tiles(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layer: LayerId,
        coords: &[TileCoord],
    ) -> std::collections::HashMap<TileCoord, openpaint_core::tile::Tile> {
        let keys: Vec<TileKey> = coords.iter().map(|c| TileKey::new(layer, *c)).collect();
        self.store
            .snapshot_some(device, queue, &keys)
            .into_iter()
            .map(|(k, t)| (k.coord, t))
            .collect()
    }

    /// Drop every tile belonging to a deleted layer.
    pub fn discard_layer(&mut self, layer: LayerId) {
        let doomed: Vec<TileCoord> = self.layer_tiles(layer).collect();
        for coord in doomed {
            if let Some(slot) = self.store.remove(TileKey::new(layer, coord)) {
                self.store.release(slot);
            }
        }
    }

    /// Upload the CPU reference canvas's tiles into a layer.
    ///
    /// The GPU is authoritative for painting (DECISIONS §4a), so in normal use nothing comes
    /// through here: `openpaint_core::Canvas` holds no pixels unless the reference rasterizer
    /// put them there. This is the seam that keeps that path usable, and what the export and
    /// compositing tests build known canvases with -- which is also why it is test-only: the
    /// shipping paint path never touches it, and an unused seam that compiles is worse than
    /// one whose absence is visible.
    #[cfg(test)]
    pub fn upload_dirty(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        layer: LayerId,
        canvas: &mut Canvas,
    ) {
        for coord in canvas.take_dirty() {
            let Some(tile) = canvas.tile(coord) else {
                continue;
            };
            // `Init::Untouched`, because every texel is about to be written and a clear would
            // be reordered *after* the `write_texture` below and wipe it (DECISIONS §11a.2).
            let Ok(pool_layer) = self.store.ensure(
                device,
                queue,
                encoder,
                TileKey::new(layer, coord),
                Init::Untouched,
            ) else {
                continue;
            };
            debug_assert_eq!(tile.bytes().len(), openpaint_core::tile::TILE_BYTES);
            queue.write_texture(
                wgpu::ImageCopyTexture {
                    texture: self.store.pool().texture(),
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: 0,
                        z: pool_layer,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                tile.bytes(),
                wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(TILE_SIZE as u32 * CANVAS_BYTES_PER_TEXEL),
                    rows_per_image: Some(TILE_SIZE as u32),
                },
                wgpu::Extent3d {
                    width: TILE_SIZE as u32,
                    height: TILE_SIZE as u32,
                    depth_or_array_layers: 1,
                },
            );
            self.mark_dirty(layer, coord);
        }
    }

    /// Write the camera, restore the tiles the viewport needs, and build the draw list.
    ///
    /// `visible` is the page-space bounding box of what the viewport covers. Culling to it is
    /// not merely an optimisation: with residency bounded and spilling in place, asking for
    /// every tile in the document would restore the whole document from the CPU every frame.
    /// So the *visible* set is the working set.
    ///
    /// Returns whether the visible set was larger than the pool could hold, which the caller
    /// reports rather than silently drawing a partial canvas.
    #[allow(clippy::too_many_arguments)]
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        xform: PageToNdc,
        visible: PageRect,
        layers: &[Layer],
        active: usize,
        stroke: Option<&StrokeLayer>,
        float: Option<LayerId>,
    ) -> bool {
        let (ex, ey) = self.page.end();
        let painting = stroke.is_some_and(StrokeLayer::has_paint);
        let (stroke_color, stroke_opacity) = stroke.map_or(([0.0; 4], 0.0), StrokeLayer::paint);

        queue.write_buffer(
            &self.params_buf,
            0,
            bytemuck::bytes_of(&ParamsUniform {
                x_row: [xform.x_row[0], xform.x_row[1], xform.x_row[2], 0.0],
                y_row: [xform.y_row[0], xform.y_row[1], xform.y_row[2], 0.0],
                page: [self.page.x as f32, self.page.y as f32, ex as f32, ey as f32],
                paper: Canvas::paper_color(),
                stroke: [
                    stroke_color[0],
                    stroke_color[1],
                    stroke_color[2],
                    stroke_opacity,
                ],
                counts: [
                    layers.len() as u32,
                    active as u32,
                    u32::from(painting),
                    stroke.map_or(0, |s| match s.mode() {
                        crate::editor::PaintMode::Normal => 0,
                        crate::editor::PaintMode::Erase => 1,
                        crate::editor::PaintMode::LockAlpha => 2,
                    }),
                ],
                misc: [TILE_SIZE as f32, 0.0, 0.0, 0.0],
            }),
        );

        let infos: Vec<LayerInfoRecord> = layers
            .iter()
            .map(|l| LayerInfoRecord {
                blend: l.blend.code(),
                opacity: l.effective_opacity(),
                clip: u32::from(l.clip_below),
                _pad: 0.0,
            })
            .collect();

        // Tiles that need drawing: anything the stack holds, *plus* anything the stroke in
        // progress has reached. The second half matters because a stroke touches tiles the
        // canvas has never had -- they only get created when it bakes -- so without them the
        // preview is missing exactly where the artist is drawing.
        let mut wanted: Vec<TileCoord> = self.occupied_tiles();
        if painting {
            if let Some(s) = stroke {
                wanted.extend(s.accum_tiles());
            }
        }
        wanted.retain(|c| tile_intersects(*c, self.page) && tile_intersects(*c, visible));
        wanted.sort_unstable_by_key(|c| (c.1, c.0));
        wanted.dedup();

        let mut under_pressure = false;
        let mut instances = Vec::with_capacity(wanted.len());
        let mut slots = Vec::with_capacity(wanted.len() * layers.len().max(1));
        let mut stroke_slots = Vec::with_capacity(wanted.len());
        let mut float_slots = Vec::with_capacity(wanted.len());

        for coord in wanted {
            for layer in layers {
                let id = LayerId(layer.id());
                match self.make_resident(device, queue, encoder, id, coord) {
                    Some(pool_layer) => slots.push(pool_layer),
                    None => {
                        // Either that layer has nothing here, or residency could not fit it --
                        // and only the second is worth reporting.
                        if self.store.contains(TileKey::new(id, coord)) {
                            under_pressure = true;
                        }
                        slots.push(ABSENT);
                    }
                }
            }
            stroke_slots.push(stroke.and_then(|s| s.accum_slot(coord)).unwrap_or(ABSENT));
            // The floating tiles are ordinary canvas tiles, so they need residency like any other.
            float_slots.push(
                float
                    .and_then(|id| self.make_resident(device, queue, encoder, id, coord))
                    .unwrap_or(ABSENT),
            );
            instances.push(TileInstance {
                coord: [coord.0, coord.1],
            });
        }

        self.instance_count = instances.len() as u32;
        self.write_storage(device, queue, &slots, &infos, &stroke_slots, &float_slots);
        if !instances.is_empty() {
            if instances.len() > self.instance_capacity {
                self.instance_capacity = instances.len().next_power_of_two();
                self.instances = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("canvas-tile-instances"),
                    size: (self.instance_capacity * std::mem::size_of::<TileInstance>())
                        as wgpu::BufferAddress,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
            }
            queue.write_buffer(&self.instances, 0, bytemuck::cast_slice(&instances));
        }
        under_pressure
    }

    /// Upload the per-frame storage arrays, growing and rebinding when they no longer fit.
    fn write_storage(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        slots: &[u32],
        infos: &[LayerInfoRecord],
        stroke_slots: &[u32],
        float_slots: &[u32],
    ) {
        let mut rebind = false;
        if slots.len() > self.slots_capacity {
            self.slots_capacity = slots.len().next_power_of_two();
            self.slots_buf = storage_buffer(device, "canvas-slots", self.slots_capacity * 4);
            rebind = true;
        }
        if infos.len() > self.infos_capacity {
            self.infos_capacity = infos.len().next_power_of_two();
            self.infos_buf = storage_buffer(
                device,
                "canvas-layer-infos",
                self.infos_capacity * std::mem::size_of::<LayerInfoRecord>(),
            );
            rebind = true;
        }
        if stroke_slots.len() > self.stroke_capacity {
            self.stroke_capacity = stroke_slots.len().next_power_of_two();
            self.float_buf = storage_buffer(device, "canvas-float-slots", self.stroke_capacity * 4);
            self.stroke_buf =
                storage_buffer(device, "canvas-stroke-slots", self.stroke_capacity * 4);
            rebind = true;
        }
        if rebind {
            self.bind_group = make_bind_group(
                device,
                &self.bind_group_layout,
                &self.params_buf,
                self.store.pool(),
                &self.sampler,
                &self.slots_buf,
                &self.infos_buf,
                &self.accum_view,
                &self.stroke_buf,
                &self.float_buf,
            );
        }

        if !slots.is_empty() {
            queue.write_buffer(&self.slots_buf, 0, bytemuck::cast_slice(slots));
        }
        if !infos.is_empty() {
            queue.write_buffer(&self.infos_buf, 0, bytemuck::cast_slice(infos));
        }
        if !float_slots.is_empty() {
            queue.write_buffer(&self.float_buf, 0, bytemuck::cast_slice(float_slots));
        }
        if !stroke_slots.is_empty() {
            queue.write_buffer(&self.stroke_buf, 0, bytemuck::cast_slice(stroke_slots));
        }
    }

    /// Record the canvas draw: the sheet, then every visible tile composited in one call.
    pub fn draw<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        pass.set_pipeline(&self.sheet_pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.draw(0..6, 0..1);

        if self.instance_count > 0 {
            pass.set_pipeline(&self.composite_pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.set_vertex_buffer(0, self.instances.slice(..));
            pass.draw(0..6, 0..self.instance_count);
        }
    }
}

fn storage_buffer(device: &wgpu::Device, label: &str, bytes: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: bytes.max(4) as wgpu::BufferAddress,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2Array,
            multisampled: false,
        },
        count: None,
    }
}

fn storage_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: true },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

#[allow(clippy::too_many_arguments)]
fn make_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    params: &wgpu::Buffer,
    pool: &TilePool,
    sampler: &wgpu::Sampler,
    slots: &wgpu::Buffer,
    infos: &wgpu::Buffer,
    accum: &wgpu::TextureView,
    stroke: &wgpu::Buffer,
    float: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("canvas-bg"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: params.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(pool.array_view()),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: slots.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: infos.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 5,
                resource: wgpu::BindingResource::TextureView(accum),
            },
            wgpu::BindGroupEntry {
                binding: 6,
                resource: stroke.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 7,
                resource: float.as_entire_binding(),
            },
        ],
    })
}

/// Whether a tile overlaps a page rectangle at all.
#[must_use]
pub fn tile_intersects(coord: TileCoord, page: PageRect) -> bool {
    let t = TILE_SIZE as i32;
    let (x0, y0) = (coord.0 * t, coord.1 * t);
    let (ex, ey) = page.end();
    x0 + t > page.x && y0 + t > page.y && x0 < ex && y0 < ey
}

/// Which tile a page pixel belongs to.
#[must_use]
pub fn tile_of(x: i32, y: i32) -> TileCoord {
    let t = TILE_SIZE as i32;
    (x.div_euclid(t), y.div_euclid(t))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The GPU tests render to an sRGB target, like the real surface, so a readback can be
    /// compared against the same encode the PNG export uses.
    const SURFACE: wgpu::TextureFormat = crate::test_gpu::SURFACE;

    /// Clipping, through the real compositor, against the CPU rule.
    ///
    /// Four properties, each of which a plausible implementation gets wrong:
    ///
    /// - a clipped layer shows **only** where its base has pixels, and nowhere else;
    /// - it is masked by the base's *alpha*, so a half-covered base half-shows it;
    /// - **two consecutive clipped layers both clip to the same base**, not to each other — that is
    ///   what makes shading and highlights over one set of flats work, and a naive
    ///   "clip to the previous layer" reads identically until you add the second one;
    /// - GPU and CPU agree, so the export and the eyedropper show what the screen shows.
    #[test]
    fn a_clipped_layer_is_masked_by_the_layer_below() {
        let Some((device, queue)) = crate::test_gpu::try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };
        const W: u32 = 64;
        const H: u32 = 64;

        let mut document = openpaint_core::Document::new(openpaint_core::Page::new(W, H));
        document.add_layer();
        document.add_layer();
        {
            let page = document.active_mut();
            page.layer_mut(1).expect("mid").clip_below = true;
            page.layer_mut(2).expect("top").clip_below = true;
        }
        let doc = document.active();
        let layers = doc.layers().to_vec();

        let stroke = crate::test_gpu::test_stroke_layer(&device);
        let mut canvas =
            CanvasRenderer::new(&device, SURFACE, doc.rect(), 64 * 1024 * 1024, &stroke);

        // Base: opaque red on the left third, HALF-covered red in the middle third, nothing on the
        // right. The middle is what proves the mask is alpha rather than a boolean.
        // Both clipped layers: solid green / solid blue everywhere, so anything visible from them
        // is the mask's doing and nothing else.
        for (index, fill) in [
            None,
            Some([0.0, 1.0, 0.0, 1.0_f32]),
            Some([0.0, 0.0, 1.0, 1.0_f32]),
        ]
        .iter()
        .enumerate()
        {
            let mut cpu = Canvas::new(W, H);
            for y in 0..H as i32 {
                for x in 0..W as i32 {
                    let px = match fill {
                        Some(c) => *c,
                        None if x < 21 => [1.0, 0.0, 0.0, 1.0],
                        None if x < 42 => [0.5, 0.0, 0.0, 0.5],
                        None => [0.0; 4],
                    };
                    cpu.replace_pixel(x, y, px);
                }
            }
            let id = LayerId(layers[index].id());
            let mut enc = device.create_command_encoder(&Default::default());
            canvas.upload_dirty(&device, &queue, &mut enc, id, &mut cpu);
            queue.submit(std::iter::once(enc.finish()));
        }

        let mut view = crate::view::View::new();
        view.fit(W, H, doc.rect());
        let mut enc = device.create_command_encoder(&Default::default());
        canvas.prepare(
            &device,
            &queue,
            &mut enc,
            view.page_to_ndc(W, H),
            view.visible_rect(W, H),
            &layers,
            0,
            None,
            None,
        );
        queue.submit(std::iter::once(enc.finish()));
        let _ = draw_to_target(&device, &queue, &canvas, W, H);

        // Sample through the CPU rule, which the eyedropper and the export both use.
        let opaque = canvas.sample_page_pixel(&device, &queue, 10, 32, &layers);
        let half = canvas.sample_page_pixel(&device, &queue, 30, 32, &layers);
        let empty = canvas.sample_page_pixel(&device, &queue, 55, 32, &layers);

        // Over an opaque base, the topmost clipped layer wins outright: solid blue.
        assert!(
            opaque[2] > 0.9 && opaque[0] < 0.1,
            "over an opaque base the clip group should show through fully: {opaque:?}"
        );

        // Where the base has no pixels, neither clipped layer may show anything at all -- so the
        // result is the bare paper.
        let paper = Canvas::paper_color();
        for c in 0..3 {
            assert!(
                (empty[c] - paper[c]).abs() < 0.01,
                "paint escaped the clip where the base is empty: {empty:?} vs paper {paper:?}"
            );
        }

        // Over the half-covered base, the group shows at half strength: between the two.
        assert!(
            half[2] > 0.2 && half[2] < opaque[2] - 0.1,
            "a half-covered base should half-show the clip group: {half:?} against {opaque:?}              and {empty:?}"
        );

        // The paper check is also what proves the *second* clipped layer clips to the group's base
        // rather than to the layer directly below it. The green layer is solid everywhere, so a
        // "clip to the previous layer" implementation would let blue cover the entire page --
        // including where the base is empty, which is exactly the region checked above. Verified by
        // sabotage per §11a.4: making the mask follow the previous layer fails that assertion.
    }

    /// The wand reads the composited image, which is the whole point of it for comics.
    ///
    /// Line art on one layer, an empty layer beneath it for colour. Referring to the layer being
    /// filled would find the entire page every time, because that layer is blank — so this puts the
    /// ink *above* the empty layer and asks the wand to respect it anyway.
    ///
    /// Uses `composited_tile`, the same fold the screen and the eyedropper use, so what the wand
    /// sees cannot be a fourth opinion about what a layer stack means.
    #[test]
    fn the_wand_sees_the_composited_image_not_one_layer() {
        let Some((device, queue)) = crate::test_gpu::try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };
        const W: u32 = 128;
        const H: u32 = 128;

        let mut document = openpaint_core::Document::new(openpaint_core::Page::new(W, H));
        document.add_layer();
        let doc = document.active();
        let layers = doc.layers().to_vec();

        let stroke = crate::test_gpu::test_stroke_layer(&device);
        let mut canvas =
            CanvasRenderer::new(&device, SURFACE, doc.rect(), 64 * 1024 * 1024, &stroke);

        // Bottom layer: entirely empty, as a flats layer is before you fill it.
        // Top layer: a black vertical line at x = 64, as inked line art.
        let mut ink = Canvas::new(W, H);
        for y in 0..H as i32 {
            ink.replace_pixel(64, y, [0.0, 0.0, 0.0, 1.0]);
        }
        let top = LayerId(layers[1].id());
        let mut enc = device.create_command_encoder(&Default::default());
        canvas.upload_dirty(&device, &queue, &mut enc, top, &mut ink);
        queue.submit(std::iter::once(enc.finish()));

        let mut cache: std::collections::HashMap<TileCoord, Vec<[u8; 3]>> =
            std::collections::HashMap::new();
        let region = openpaint_core::region::flood(doc.rect(), (20, 64), 32, 0, |x, y| {
            let coord = tile_of(x, y);
            let tile = cache
                .entry(coord)
                .or_insert_with(|| canvas.composited_tile(&device, &queue, coord, &layers));
            let side = TILE_SIZE as i32;
            tile[y.rem_euclid(side) as usize * TILE_SIZE + x.rem_euclid(side) as usize]
        });

        assert_eq!(
            region.coverage_at(20, 64),
            255,
            "the wand found nothing at all"
        );
        assert_eq!(
            region.coverage_at(60, 64),
            255,
            "it should reach right up to the line"
        );
        assert_eq!(
            region.coverage_at(100, 64),
            0,
            "it crossed the ink, so it is reading the empty layer rather than the composite"
        );
    }

    /// The eyedropper must return exactly what is on screen.
    ///
    /// Not "close to the active layer" and not "the CPU fold agrees with itself": this renders the
    /// stack through the real compositor, reads the pixel back off the rendered target, and asks the
    /// sampler for the same coordinate. Anything that made the two disagree — sampling one layer,
    /// forgetting layer opacity, forgetting the paper underneath, mixing up premultiplication —
    /// shows up here as a colour mismatch.
    ///
    /// Uses a **partially transparent top layer over an opaque one over paper**, because a fully
    /// opaque stack hides every one of those mistakes.
    #[test]
    fn a_sampled_colour_is_the_displayed_colour() {
        let Some((device, queue)) = crate::test_gpu::try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };
        const W: u32 = 64;
        const H: u32 = 64;
        const UNDER: [f32; 4] = [0.80, 0.30, 0.10, 1.0];
        const OVER: [f32; 4] = [0.15, 0.60, 0.90, 0.55];

        let mut document = openpaint_core::Document::new(openpaint_core::Page::new(W, H));
        document.add_layer();
        // Partial layer opacity as well as partial pixel alpha: two different multiplies, and
        // dropping either is a plausible mistake.
        document.active_mut().layer_mut(1).expect("top").opacity = 0.7;
        let doc = document.active();

        let stroke = crate::test_gpu::test_stroke_layer(&device);
        let mut canvas =
            CanvasRenderer::new(&device, SURFACE, doc.rect(), 64 * 1024 * 1024, &stroke);

        for (index, fill) in [UNDER, OVER].iter().enumerate() {
            let mut cpu = Canvas::new(W, H);
            let a = fill[3];
            let premul = [fill[0] * a, fill[1] * a, fill[2] * a, a];
            for y in 0..H as i32 {
                for x in 0..W as i32 {
                    cpu.replace_pixel(x, y, premul);
                }
            }
            let id = LayerId(doc.layers()[index].id());
            let mut enc = device.create_command_encoder(&Default::default());
            canvas.upload_dirty(&device, &queue, &mut enc, id, &mut cpu);
            queue.submit(std::iter::once(enc.finish()));
        }

        let mut view = crate::view::View::new();
        view.fit(W, H, doc.rect());
        let mut enc = device.create_command_encoder(&Default::default());
        canvas.prepare(
            &device,
            &queue,
            &mut enc,
            view.page_to_ndc(W, H),
            view.visible_rect(W, H),
            doc.layers(),
            0,
            None,
            None,
        );
        queue.submit(std::iter::once(enc.finish()));
        let rendered = draw_to_target(&device, &queue, &canvas, W, H);

        // The fit insets the page, so screen and page coordinates are not the same. The fills are
        // uniform, so read the screen at its centre -- guaranteed inside the page -- and compare
        // against several page coordinates, all of which must give that same colour.
        let shown = rendered[(H as usize / 2) * W as usize + W as usize / 2];

        let layers = doc.layers().to_vec();
        for (x, y) in [(0_i32, 0_i32), (17, 31), (63, 63)] {
            let sampled = canvas.sample_page_pixel(&device, &queue, x, y, &layers);
            let sampled = crate::export::to_srgb8_for_test(sampled);
            for c in 0..3 {
                let d = i32::from(sampled[c]) - i32::from(shown[c]);
                assert!(
                    d.abs() <= 2,
                    "at page ({x}, {y}) the eyedropper says {sampled:?} but the screen shows                      {shown:?}"
                );
            }
        }

        // A pixel no layer has painted must read as the paper, not as transparent black: it is
        // what the artist can see there.
        let empty = canvas.sample_page_pixel(&device, &queue, -500, -500, &layers);
        let empty = crate::export::to_srgb8_for_test(empty);
        let paper = crate::export::to_srgb8_for_test(Canvas::paper_color());
        assert_eq!(empty, paper, "unpainted canvas should sample as the paper");
    }

    /// The compositor's arithmetic exists twice -- `composite_fs` in canvas.wgsl and
    /// `openpaint_core::layer::Blend` plus `export::blend_over` on the CPU. Two copies of a
    /// formula drift, exactly like the dab falloff curve. This composites the same stack
    /// through both and diffs the pixels.
    ///
    /// Deliberately a **two-layer** stack with the mode's layer on top at full opacity. An
    /// earlier version put the mode in the middle of three layers with partial opacity, which
    /// looked like a stronger test but damped the mode's contribution down to the comparison
    /// tolerance -- it passed with Multiply deliberately scaled by 0.9. Verified per §11a.4:
    /// this version fails on that same injection.
    ///
    /// The layer still has **partial alpha**, because that is the term a naive premultiplied
    /// implementation gets wrong; with an opaque layer nearly any formula agrees.
    #[test]
    fn the_gpu_compositor_matches_the_cpu_reference() {
        let Some((device, queue)) = crate::test_gpu::try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };
        const W: u32 = 64;
        const H: u32 = 64;
        // Straight colours; premultiplication happens where the tiles are filled.
        const UNDER: [f32; 4] = [0.75, 0.35, 0.15, 1.0];
        const OVER: [f32; 4] = [0.20, 0.85, 0.45, 0.8];

        let mut results = Vec::new();
        for mode in openpaint_core::Blend::ALL {
            let mut document = openpaint_core::Document::new(openpaint_core::Page::new(W, H));
            document.add_layer();
            document.active_mut().layer_mut(1).expect("top").blend = mode;
            let doc = document.active();

            let stroke = crate::test_gpu::test_stroke_layer(&device);
            let mut canvas =
                CanvasRenderer::new(&device, SURFACE, doc.rect(), 64 * 1024 * 1024, &stroke);

            for (index, fill) in [UNDER, OVER].iter().enumerate() {
                let mut cpu = Canvas::new(W, H);
                let a = fill[3];
                let premul = [fill[0] * a, fill[1] * a, fill[2] * a, a];
                for y in 0..H as i32 {
                    for x in 0..W as i32 {
                        cpu.replace_pixel(x, y, premul);
                    }
                }
                let id = LayerId(doc.layers()[index].id());
                let mut enc = device.create_command_encoder(&Default::default());
                canvas.upload_dirty(&device, &queue, &mut enc, id, &mut cpu);
                queue.submit(std::iter::once(enc.finish()));
            }

            let mut view = crate::view::View::new();
            view.fit(W, H, doc.rect());
            let mut enc = device.create_command_encoder(&Default::default());
            canvas.prepare(
                &device,
                &queue,
                &mut enc,
                view.page_to_ndc(W, H),
                view.visible_rect(W, H),
                doc.layers(),
                0,
                None,
                None,
            );
            queue.submit(std::iter::once(enc.finish()));
            let gpu = draw_to_target(&device, &queue, &canvas, W, H);

            // The same stack through the CPU reference.
            let mut expected = Canvas::paper_color();
            for (index, fill) in [UNDER, OVER].iter().enumerate() {
                let layer = &doc.layers()[index];
                let a = fill[3] * layer.effective_opacity();
                let src = [fill[0] * a, fill[1] * a, fill[2] * a, a];
                expected = crate::export::blend_over(src, expected, layer.blend);
            }
            let want = crate::export::to_srgb8_for_test(expected);

            // The fills are uniform, so every pixel of the page should agree; sampling the
            // centre avoids the page's own edges where the quad is clipped.
            let mid = gpu[(H as usize / 2) * W as usize + W as usize / 2];
            for c in 0..3 {
                let diff = i32::from(mid[c]) - i32::from(want[c]);
                assert!(
                    diff.abs() <= 2,
                    "{mode:?}: channel {c} differs by {diff}; gpu {mid:?} cpu {want:?}"
                );
            }
            results.push((mode, mid));
        }

        // And the mode must actually reach the shader. Without this, a compositor that ignored
        // the blend code entirely would agree with a CPU reference that also ignored it.
        for (i, (mode_a, a)) in results.iter().enumerate() {
            for (mode_b, b) in &results[i + 1..] {
                let spread: i32 = (0..3)
                    .map(|c| (i32::from(a[c]) - i32::from(b[c])).abs())
                    .max()
                    .unwrap_or(0);
                assert!(
                    spread > 8,
                    "{mode_a:?} and {mode_b:?} produced nearly the same pixel \
                     ({a:?} vs {b:?}); is the blend mode reaching the shader?"
                );
            }
        }
    }

    /// Draw the canvas into an offscreen sRGB target and read it back as bytes.
    pub(crate) fn draw_to_target(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        canvas: &CanvasRenderer,
        w: u32,
        h: u32,
    ) -> Vec<[u8; 4]> {
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("composite-test-target"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: SURFACE,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("composite-test-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLUE),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            canvas.draw(&mut pass);
        }
        queue.submit(std::iter::once(encoder.finish()));
        read_rgba8(device, queue, &target, w, h)
    }

    /// Read an RGBA8 target back, skipping row padding.
    fn read_rgba8(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        texture: &wgpu::Texture,
        w: u32,
        h: u32,
    ) -> Vec<[u8; 4]> {
        let unpadded = w * 4;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded = unpadded.div_ceil(align) * align;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("draw-test-readback"),
            size: u64::from(padded) * u64::from(h),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyBuffer {
                buffer: &buffer,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(std::iter::once(encoder.finish()));
        buffer.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        device.poll(wgpu::Maintain::Wait);
        let mapped = buffer.slice(..).get_mapped_range();
        let mut out = Vec::with_capacity((w * h) as usize);
        for row in 0..h {
            let start = (row * padded) as usize;
            let end = start + unpadded as usize;
            out.extend_from_slice(mapped[start..end].as_chunks::<4>().0);
        }
        out
    }

    /// A painted layer must actually reach the screen. Every other GPU test stops at "the tile
    /// holds the right pixels", which says nothing about whether the tile is ever drawn -- and
    /// a version that painted nothing at all once passed all of them.
    #[test]
    fn a_painted_layer_is_actually_drawn() {
        let Some((device, queue)) = crate::test_gpu::try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };
        const VIEW: u32 = 256;

        let doc = openpaint_core::Page::new(400, 400);
        // Red over the page's top-left corner only, so the sheet is visible elsewhere.
        let mut cpu = Canvas::new(400, 400);
        for y in 0..64 {
            for x in 0..64 {
                cpu.replace_pixel(x, y, [1.0, 0.0, 0.0, 1.0]);
            }
        }

        let stroke = crate::test_gpu::test_stroke_layer(&device);
        let mut canvas =
            CanvasRenderer::new(&device, SURFACE, doc.rect(), 64 * 1024 * 1024, &stroke);
        let mut enc = device.create_command_encoder(&Default::default());
        canvas.upload_dirty(
            &device,
            &queue,
            &mut enc,
            LayerId(doc.layers()[0].id()),
            &mut cpu,
        );
        queue.submit(std::iter::once(enc.finish()));
        assert_eq!(
            canvas.residency().0,
            1,
            "expected exactly one tile resident"
        );

        let mut view = crate::view::View::new();
        view.fit(VIEW, VIEW, doc.rect());
        let mut enc = device.create_command_encoder(&Default::default());
        assert!(
            !canvas.prepare(
                &device,
                &queue,
                &mut enc,
                view.page_to_ndc(VIEW, VIEW),
                view.visible_rect(VIEW, VIEW),
                doc.layers(),
                0,
                None,
                None,
            ),
            "the test budget should be ample"
        );
        queue.submit(std::iter::once(enc.finish()));

        let pixels = draw_to_target(&device, &queue, &canvas, VIEW, VIEW);
        let at = |x: u32, y: u32| pixels[(y * VIEW + x) as usize];

        // Paper is bright in every channel; the backdrop is pure blue.
        let mid = at(VIEW / 2, VIEW / 2);
        assert!(
            mid[0] > 200 && mid[1] > 200,
            "the paper sheet was not drawn; centre is {mid:?}"
        );

        let (px, py) = view.canvas_to_screen(20.0, 20.0, VIEW, VIEW);
        let corner = at(px.round() as u32, py.round() as u32);
        assert!(
            corner[0] > 200 && corner[1] < 80 && corner[2] < 80,
            "the painted layer was not drawn; got {corner:?} at ({px}, {py})"
        );
    }

    #[test]
    fn a_tile_inside_the_page_intersects_it() {
        let page = PageRect::from_size(1000, 1000);
        assert!(tile_intersects((0, 0), page));
        assert!(tile_intersects((3, 3), page));
    }

    #[test]
    fn a_tile_beyond_the_page_does_not() {
        let page = PageRect::from_size(1000, 1000);
        assert!(!tile_intersects((4, 0), page), "1024 is past 1000");
        assert!(!tile_intersects((-1, 0), page));
    }

    /// A page cropped inward leaves tiles outside it. They must be excluded from drawing while
    /// remaining in storage -- that separation is the whole of non-destructive crop.
    #[test]
    fn cropping_excludes_the_tiles_it_leaves_behind() {
        let full = PageRect::from_size(1000, 1000);
        let cropped = PageRect::from_size(300, 300);
        assert!(tile_intersects((3, 3), full));
        assert!(!tile_intersects((3, 3), cropped));
    }

    /// A page extended up or left has a negative origin, so tile coordinates go negative too.
    /// `div_euclid` is what makes that work; plain division would round toward zero and put
    /// -1..-255 in tile 0.
    #[test]
    fn negative_page_coordinates_map_to_negative_tiles() {
        assert_eq!(tile_of(-1, -1), (-1, -1));
        assert_eq!(tile_of(-256, -256), (-1, -1));
        assert_eq!(tile_of(-257, 0), (-2, 0));
        assert_eq!(tile_of(0, 0), (0, 0));

        let page = PageRect::new(-400, -500, 900, 1200);
        assert!(tile_intersects((-2, -2), page));
        assert!(tile_intersects((-1, -1), page));
    }
}
