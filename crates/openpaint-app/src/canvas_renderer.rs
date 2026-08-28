//! Draws the canvas from a bounded pool of GPU tiles.
//!
//! # What replaced what
//!
//! This used to hold one texture the size of the whole page. That was recorded as a
//! Phase-0 shortcut (OPEN_QUESTIONS Q13) and it imposed two ceilings the app had to
//! apologise for: a page could be no bigger than `max_texture_dimension_2d` (8192), and
//! no bigger than one allocation the driver would accept (~16 Mpx at `Rgba16Float`).
//! Both are gone: a page's size no longer determines any allocation.
//!
//! Three consequences worth stating, because they are the point:
//!
//! 1. **Memory scales with painted area, not page area.** A blank 800×20000 webtoon strip
//!    costs nothing until it is drawn on.
//! 2. **Storage is decoupled from the page rectangle**, which is what makes a crop
//!    non-destructive (DECISIONS §5c): tiles outside the page are kept, just not drawn.
//! 3. **Resizing is metadata.** No texture is recreated and no pixel is copied — the
//!    previous implementation did a full reallocation and a whole-canvas blit per resize.
//!
//! # Page coordinates go all the way down
//!
//! There is no longer a page-origin-to-texture-origin offset anywhere, because a tile is
//! addressed by its *page* tile coordinate. The signed-origin invariant (DECISIONS §5a) —
//! a pixel keeps its coordinate forever — now holds unbroken from the core to the GPU,
//! and the subtraction that used to sit at the boundary is deleted rather than moved.

use half::f16;
use openpaint_core::tile::{TileCoord, TILE_BYTES, TILE_CHANNELS, TILE_SIZE};
use openpaint_core::{Canvas, PageRect};

use crate::tile_pool::{Slot, TilePool};
use crate::tile_store::{Init, LayerId, Pressure, TileKey, TileStore};
use crate::view::PageToNdc;

/// Pixel format of canvas tiles: linear premultiplied RGBA f16, matching
/// `openpaint_core::tile` exactly (DECISIONS §4b) so uploads are a straight byte copy.
pub const CANVAS_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// Bytes per canvas texel (RGBA f16).
pub const CANVAS_BYTES_PER_TEXEL: u32 = (TILE_CHANNELS * std::mem::size_of::<f16>()) as u32;

/// The layer being drawn and painted.
///
/// One layer for now. Named rather than hardcoded at every call site so that adding the
/// stack is a change to who supplies it, not a hunt through the file.
pub const ACTIVE_LAYER: LayerId = LayerId(0);

/// Uniform matching `Xform` in canvas.wgsl.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct XformUniform {
    x_row: [f32; 4],
    y_row: [f32; 4],
    page: [f32; 4],
    paper: [f32; 4],
    params: [f32; 4],
}

/// Per-instance data matching `TileInst` in canvas.wgsl.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TileInstance {
    coord: [i32; 2],
    layer: u32,
    _pad: u32,
}

pub struct CanvasRenderer {
    store: TileStore,
    /// The page rectangle. Drawing and painting are bounded by it; storage is not.
    page: PageRect,
    bind_group: wgpu::BindGroup,
    sheet_pipeline: wgpu::RenderPipeline,
    tile_pipeline: wgpu::RenderPipeline,
    xform_buf: wgpu::Buffer,
    instances: wgpu::Buffer,
    instance_capacity: usize,
    /// Instances written by the last `prepare`, i.e. what `draw` will draw.
    instance_count: u32,
}

impl CanvasRenderer {
    pub fn new(
        device: &wgpu::Device,
        surface_format: wgpu::TextureFormat,
        canvas: &Canvas,
        budget_bytes: u64,
    ) -> Self {
        let store = TileStore::new(device, budget_bytes);

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("canvas-sampler"),
            // Nearest when magnifying, so zooming in shows real pixels rather than a
            // blur -- what you want when inspecting brush edges. Linear when minifying,
            // so a zoomed-out canvas doesn't alias into noise.
            //
            // An array texture is what makes this safe: each tile is its own image with
            // its own clamped edges, so a filtered sample near a tile border cannot pull
            // in an unrelated tile the way it would from a 2D atlas.
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let xform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("canvas-xform"),
            size: std::mem::size_of::<XformUniform>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
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
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let bind_group = make_bind_group(
            device,
            &bind_group_layout,
            &xform_buf,
            store.pool(),
            &sampler,
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

        let tile_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("canvas-tile-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "tile_vs",
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<TileInstance>() as wgpu::BufferAddress,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &[
                        wgpu::VertexAttribute {
                            offset: 0,
                            shader_location: 0,
                            format: wgpu::VertexFormat::Sint32x2,
                        },
                        wgpu::VertexAttribute {
                            offset: 8,
                            shader_location: 1,
                            format: wgpu::VertexFormat::Uint32,
                        },
                    ],
                }],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "tile_fs",
                targets: &[target(surface_format)],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let instance_capacity = 128;
        let instances = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("canvas-tile-instances"),
            size: (instance_capacity * std::mem::size_of::<TileInstance>()) as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            store,
            page: canvas.rect(),
            bind_group,
            sheet_pipeline,
            tile_pipeline,
            xform_buf,
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
    /// The entire cost of a resize. Tiles are addressed in page coordinates, so nothing
    /// is reallocated, copied, or rekeyed — and tiles that fall outside the new rectangle
    /// are **kept**, which is what makes a crop non-destructive (DECISIONS §5c).
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

    /// The pool layer holding `coord`, if it is resident.
    #[must_use]
    pub fn slot(&self, coord: TileCoord) -> Option<&Slot> {
        self.store.slot(TileKey::new(ACTIVE_LAYER, coord))
    }

    /// Every tile the canvas holds, resident or spilled.
    pub fn tiles(&self) -> impl Iterator<Item = TileCoord> + '_ {
        self.store
            .keys()
            .filter(|k| k.layer == ACTIVE_LAYER)
            .map(|k| k.coord)
    }

    /// Make sure `coord` is on the GPU, restoring it from the CPU if it had spilled.
    ///
    /// Returns `None` when it is not a tile this canvas has, so callers that only want
    /// existing pixels do not accidentally create empty ones.
    pub fn make_resident(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        coord: TileCoord,
    ) -> Option<u32> {
        let key = TileKey::new(ACTIVE_LAYER, coord);
        if !self.store.contains(key) {
            return None;
        }
        self.store
            .ensure(
                device,
                queue,
                encoder,
                key,
                // Unreachable: the tile exists, so this is a restore, not a creation.
                Init::Clear(paper_clear_color(Canvas::paper_color())),
            )
            .ok()
    }

    /// Note that a tile's pixels changed, so residency owes it a readback if it spills.
    pub fn mark_dirty(&mut self, coord: TileCoord) {
        self.store.mark_dirty(TileKey::new(ACTIVE_LAYER, coord));
    }

    /// Make sure `coord` has a tile, clearing a fresh one to paper.
    ///
    /// Fresh tiles are cleared to paper rather than transparent so a partially-covered tile
    /// shows sheet, not a hole, in the parts the brush has not reached. That changes when
    /// layers land: a *layer* is transparent where unpainted, and the paper moves to the
    /// bottom of the compositor.
    pub fn ensure_tile(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        coord: TileCoord,
    ) -> Result<u32, Pressure> {
        self.store.ensure(
            device,
            queue,
            encoder,
            TileKey::new(ACTIVE_LAYER, coord),
            Init::Clear(paper_clear_color(Canvas::paper_color())),
        )
    }

    /// Make sure `coord` has a tile, **without** clearing it to paper, for a caller that is
    /// about to overwrite every texel.
    ///
    /// The clear is skipped rather than merely wasted. Clearing goes through the encoder while
    /// a full upload goes through `Queue::write_texture`, and every queue write in a
    /// submission is applied *before* any of its commands run -- so a paper clear here would
    /// be reordered *after* the upload and wipe it. The export test is what caught that.
    fn ensure_tile_for_full_write(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        coord: TileCoord,
    ) -> Option<u32> {
        self.store
            .ensure(
                device,
                queue,
                encoder,
                TileKey::new(ACTIVE_LAYER, coord),
                Init::Untouched,
            )
            .ok()
    }

    /// The tile at `coord` as a render target, for a stroke to bake into.
    #[must_use]
    pub fn tile_target(&self, coord: TileCoord) -> Option<&wgpu::TextureView> {
        self.store
            .slot(TileKey::new(ACTIVE_LAYER, coord))
            .map(|s| self.store.pool().layer_view(s))
    }

    /// Drop every tile that lies entirely outside the page, handing the caller their
    /// coordinates so it can record them.
    ///
    /// The only operation that discards pixels — see [`CanvasRenderer::take_tile`].
    pub fn tiles_outside_page(&self) -> Vec<TileCoord> {
        let page = self.page;
        self.tiles()
            .filter(|c| !tile_intersects(*c, page))
            .collect()
    }

    /// Take a tile out of the canvas, giving up ownership of its layer.
    ///
    /// Used by undo (a tile the stroke created must go away again) and by Trim. The layer
    /// is *not* freed here: the caller decides whether to return it to the pool or keep it
    /// alive as a snapshot, and the move-only slot is what makes that choice explicit.
    #[must_use]
    pub fn take_tile(&mut self, coord: TileCoord) -> Option<Slot> {
        self.store.remove(TileKey::new(ACTIVE_LAYER, coord))
    }

    /// Put a tile back, e.g. when undoing a Trim. Returns any displaced slot.
    #[must_use = "the displaced slot must be returned to a pool"]
    pub fn put_tile(&mut self, coord: TileCoord, slot: Slot) -> Option<Slot> {
        self.store.insert(TileKey::new(ACTIVE_LAYER, coord), slot)
    }

    /// Allocate a bare tile without clearing it, for a caller that will overwrite it.
    pub fn alloc_bare(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) -> Option<Slot> {
        self.store.alloc_bare(device, queue)
    }

    /// Return a layer to the canvas pool.
    pub fn release(&mut self, slot: Slot) {
        self.store.release(slot);
    }

    /// Upload the CPU reference canvas's dirty tiles.
    ///
    /// The GPU is authoritative for painting (DECISIONS §4a), so in normal use there is
    /// nothing here: `openpaint_core::Canvas` holds no pixels unless the CPU reference
    /// path put them there. This is the seam that keeps that path usable.
    pub fn upload_dirty(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        canvas: &mut Canvas,
    ) {
        for coord in canvas.take_dirty() {
            let Some(tile) = canvas.tile(coord) else {
                continue;
            };
            let Some(layer) = self.ensure_tile_for_full_write(device, queue, encoder, coord) else {
                continue;
            };
            debug_assert_eq!(tile.bytes().len(), TILE_BYTES);
            queue.write_texture(
                wgpu::ImageCopyTexture {
                    texture: self.store.pool().texture(),
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: 0,
                        z: layer,
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
            self.mark_dirty(coord);
        }
    }

    /// Write the camera, restore the tiles the viewport needs, and build the draw list.
    ///
    /// `visible` is the page-space bounding box of what the viewport covers. Culling to it is
    /// no longer merely an optimisation: with residency bounded and spilling in place, asking
    /// for every tile in the document would restore the whole document from the CPU every
    /// frame. So the *visible* set is the working set.
    ///
    /// Returns whether the visible set was larger than the pool could hold, which the caller
    /// reports rather than silently drawing a partial canvas.
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        xform: PageToNdc,
        visible: PageRect,
    ) -> bool {
        let (ex, ey) = self.page.end();
        let paper = Canvas::paper_color();
        queue.write_buffer(
            &self.xform_buf,
            0,
            bytemuck::bytes_of(&XformUniform {
                x_row: [xform.x_row[0], xform.x_row[1], xform.x_row[2], 0.0],
                y_row: [xform.y_row[0], xform.y_row[1], xform.y_row[2], 0.0],
                page: [self.page.x as f32, self.page.y as f32, ex as f32, ey as f32],
                paper,
                params: [TILE_SIZE as f32, 0.0, 0.0, 0.0],
            }),
        );

        // Tiles that exist, lie inside the page, and are on screen. Sorted so the frame's
        // residency requests -- and therefore its eviction order -- are deterministic.
        let mut wanted: Vec<TileCoord> = self
            .tiles()
            .filter(|c| tile_intersects(*c, self.page) && tile_intersects(*c, visible))
            .collect();
        wanted.sort_unstable_by_key(|c| (c.1, c.0));

        let mut under_pressure = false;
        let mut list = Vec::with_capacity(wanted.len());
        for coord in wanted {
            match self.make_resident(device, queue, encoder, coord) {
                Some(layer) => list.push(TileInstance {
                    coord: [coord.0, coord.1],
                    layer,
                    _pad: 0,
                }),
                None => under_pressure = true,
            }
        }

        self.instance_count = list.len() as u32;
        if list.is_empty() {
            return under_pressure;
        }
        if list.len() > self.instance_capacity {
            self.instance_capacity = list.len().next_power_of_two();
            self.instances = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("canvas-tile-instances"),
                size: (self.instance_capacity * std::mem::size_of::<TileInstance>())
                    as wgpu::BufferAddress,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        queue.write_buffer(&self.instances, 0, bytemuck::cast_slice(&list));
        under_pressure
    }

    /// Record the canvas draw: the sheet, then every visible tile in one instanced call.
    pub fn draw<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        pass.set_pipeline(&self.sheet_pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.draw(0..6, 0..1);

        if self.instance_count > 0 {
            pass.set_pipeline(&self.tile_pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.set_vertex_buffer(0, self.instances.slice(..));
            pass.draw(0..6, 0..self.instance_count);
        }
    }
}

fn make_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    xform: &wgpu::Buffer,
    pool: &TilePool,
    sampler: &wgpu::Sampler,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("canvas-bg"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: xform.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(pool.array_view()),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(sampler),
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

/// A linear premultiplied colour as a clear value.
fn paper_clear_color(rgba: [f32; 4]) -> wgpu::Color {
    wgpu::Color {
        r: f64::from(rgba[0]),
        g: f64::from(rgba[1]),
        b: f64::from(rgba[2]),
        a: f64::from(rgba[3]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// A page cropped inward leaves tiles outside it. They must be excluded from drawing
    /// while remaining in storage -- that separation is the whole of non-destructive crop.
    #[test]
    fn cropping_excludes_the_tiles_it_leaves_behind() {
        let full = PageRect::from_size(1000, 1000);
        let cropped = PageRect::from_size(300, 300);
        assert!(tile_intersects((3, 3), full));
        assert!(!tile_intersects((3, 3), cropped));
    }

    /// A page extended up or left has a negative origin, so tile coordinates go negative
    /// too. `div_euclid` is what makes that work; plain division would round toward zero
    /// and put -1..-255 in tile 0.
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

    /// Draw the canvas into an offscreen target and check what lands on screen.
    ///
    /// The other half of the paint path, and it had no coverage: every GPU test until now
    /// stopped at "the tile holds the right pixels", which says nothing about whether the
    /// tile is ever drawn. Uses a fitted `View`, so the affine, the instance buffer, the
    /// page clip and the sheet are all exercised the way a frame exercises them.
    #[test]
    fn a_painted_tile_is_actually_drawn() {
        let Some((device, queue)) = crate::test_gpu::try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };
        const SURFACE: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
        const VIEW_W: u32 = 256;
        const VIEW_H: u32 = 256;

        // A small page, painted red in one corner through the CPU reference upload.
        let mut cpu = Canvas::new(400, 400);
        let red = [1.0, 0.0, 0.0, 1.0];
        for y in 0..64 {
            for x in 0..64 {
                cpu.replace_pixel(x, y, red);
            }
        }

        let mut canvas = CanvasRenderer::new(&device, SURFACE, &cpu, 128 * 1024 * 1024);
        let mut enc = device.create_command_encoder(&Default::default());
        canvas.upload_dirty(&device, &queue, &mut enc, &mut cpu);
        queue.submit(std::iter::once(enc.finish()));
        assert_eq!(
            canvas.residency().0,
            1,
            "expected exactly one tile resident"
        );

        let mut view = crate::view::View::new();
        view.fit(VIEW_W, VIEW_H, &cpu);
        let mut enc = device.create_command_encoder(&Default::default());
        // The whole page is visible in this test, so culling must not remove anything.
        let visible = view.visible_rect(VIEW_W, VIEW_H);
        assert!(
            !canvas.prepare(
                &device,
                &queue,
                &mut enc,
                view.page_to_ndc(VIEW_W, VIEW_H),
                visible
            ),
            "the test budget should be ample"
        );
        queue.submit(std::iter::once(enc.finish()));

        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("draw-test-target"),
            size: wgpu::Extent3d {
                width: VIEW_W,
                height: VIEW_H,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: SURFACE,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("draw-test-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // Blue backdrop, so "nothing was drawn" is unmistakable.
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

        let pixels = read_rgba8(&device, &queue, &target, VIEW_W, VIEW_H);
        let at = |x: u32, y: u32| pixels[(y * VIEW_W + x) as usize];

        // The page is fitted and centred, so its own centre is the view's centre and must be
        // paper -- not the backdrop.
        // Paper is bright in every channel; the backdrop is pure blue.
        let mid = at(VIEW_W / 2, VIEW_H / 2);
        assert!(
            mid[0] > 200 && mid[1] > 200,
            "the paper sheet was not drawn; centre is {mid:?}"
        );

        // And the painted tile covers the page's top-left eighth, so a point just inside the
        // page's top-left corner must be red.
        let (px, py) = view.canvas_to_screen(20.0, 20.0, VIEW_W, VIEW_H);
        let corner = at(px.round() as u32, py.round() as u32);
        assert!(
            corner[0] > 200 && corner[1] < 80 && corner[2] < 80,
            "the painted tile was not drawn; got {corner:?} at ({px}, {py})"
        );
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
}
