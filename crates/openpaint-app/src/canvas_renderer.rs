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

use crate::tile_pool::{layers_for_budget, TileMap, TilePool};
use crate::view::PageToNdc;

/// Pixel format of canvas tiles: linear premultiplied RGBA f16, matching
/// `openpaint_core::tile` exactly (DECISIONS §4b) so uploads are a straight byte copy.
pub const CANVAS_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// Bytes per canvas texel (RGBA f16).
pub const CANVAS_BYTES_PER_TEXEL: u32 = (TILE_CHANNELS * std::mem::size_of::<f16>()) as u32;

/// GPU memory the resident canvas tiles may occupy.
///
/// Chosen for the Surface-class target (DECISIONS §2), where graphics memory is shared
/// with the system: 96 MiB is 192 tiles, enough to hold an A4 page at 300 DPI fully
/// inked (140 tiles) without asking a laptop to give up a sixth of a gigabyte.
///
/// This bounds *residency*, not canvas size. Exceeding it is a normal condition on a long
/// webtoon and is what spilling to CPU/disk answers (Q13); until that lands, the pool
/// reports exhaustion rather than pretending.
pub const CANVAS_BUDGET_BYTES: u64 = 96 * 1024 * 1024;

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
    pool: TilePool,
    map: TileMap,
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
    ) -> Self {
        let capacity = layers_for_budget(device, CANVAS_BYTES_PER_TEXEL, CANVAS_BUDGET_BYTES);
        let pool = TilePool::new(
            device,
            "canvas-tiles",
            CANVAS_FORMAT,
            capacity,
            CANVAS_BYTES_PER_TEXEL,
            // RENDER_ATTACHMENT so a stroke can bake into a tile; COPY_SRC/DST for undo
            // snapshots, export, and CPU-reference uploads.
            wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
        );

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

        let bind_group = make_bind_group(device, &bind_group_layout, &xform_buf, &pool, &sampler);

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
            pool,
            map: TileMap::default(),
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
        &self.pool
    }

    /// Resident tile count and capacity, for display.
    #[must_use]
    pub fn residency(&self) -> (u32, u32) {
        (self.pool.used(), self.pool.capacity())
    }

    /// The pool layer holding `coord`, if it is resident.
    #[must_use]
    pub fn slot(&self, coord: TileCoord) -> Option<&crate::tile_pool::Slot> {
        self.map.slot(coord)
    }

    /// Every resident tile coordinate.
    pub fn tiles(&self) -> impl Iterator<Item = TileCoord> + '_ {
        self.map.iter().map(|(c, _)| c)
    }

    /// Make sure `coord` has a tile, clearing a fresh one to paper.
    ///
    /// Returns `None` when the pool is full — a real, reachable state that the caller has
    /// to report rather than paint through. Fresh tiles are cleared to paper rather than
    /// transparent so a partially-covered tile shows sheet, not a hole, in the parts the
    /// brush has not reached.
    pub fn ensure_tile(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        coord: TileCoord,
    ) -> Option<u32> {
        if let Some(slot) = self.map.slot(coord) {
            return Some(slot.layer());
        }
        let slot = self.pool.alloc()?;
        let layer = slot.layer();
        self.pool
            .clear_layer(encoder, &slot, paper_clear_color(Canvas::paper_color()));
        debug_assert!(self.map.insert(coord, slot).is_none());
        Some(layer)
    }

    /// Make sure `coord` has a tile, **without** clearing it, for a caller that is about to
    /// overwrite every texel.
    ///
    /// Not an optimisation -- a correctness requirement. Clearing goes through the encoder
    /// while a full upload goes through `Queue::write_texture`, and every queue write in a
    /// submission is applied *before* any of its commands run. So a clear here would be
    /// reordered *after* the upload and wipe it. The test that exports a known canvas is
    /// what caught this.
    fn ensure_tile_for_full_write(&mut self, coord: TileCoord) -> Option<u32> {
        if let Some(slot) = self.map.slot(coord) {
            return Some(slot.layer());
        }
        let slot = self.pool.alloc()?;
        let layer = slot.layer();
        debug_assert!(self.map.insert(coord, slot).is_none());
        Some(layer)
    }

    /// The tile at `coord` as a render target, for a stroke to bake into.
    #[must_use]
    pub fn tile_target(&self, coord: TileCoord) -> Option<&wgpu::TextureView> {
        self.map.slot(coord).map(|s| self.pool.layer_view(s))
    }

    /// Drop every tile that lies entirely outside the page, handing the caller their
    /// coordinates so it can record them.
    ///
    /// The only operation that discards pixels — see [`CanvasRenderer::take_tile`].
    pub fn tiles_outside_page(&self) -> Vec<TileCoord> {
        self.map
            .iter()
            .map(|(c, _)| c)
            .filter(|c| !tile_intersects(*c, self.page))
            .collect()
    }

    /// Take a tile out of the canvas, giving up ownership of its layer.
    ///
    /// Used by undo (a tile the stroke created must go away again) and by Trim. The layer
    /// is *not* freed here: the caller decides whether to return it to the pool or keep it
    /// alive as a snapshot, and the move-only slot is what makes that choice explicit.
    #[must_use]
    pub fn take_tile(&mut self, coord: TileCoord) -> Option<crate::tile_pool::Slot> {
        self.map.take(coord)
    }

    /// Put a tile back, e.g. when undoing a Trim. Returns any displaced slot.
    #[must_use = "the displaced slot must be returned to a pool"]
    pub fn put_tile(
        &mut self,
        coord: TileCoord,
        slot: crate::tile_pool::Slot,
    ) -> Option<crate::tile_pool::Slot> {
        self.map.insert(coord, slot)
    }

    /// Allocate a bare tile without clearing it, for a caller that will overwrite it.
    pub fn alloc_bare(&mut self) -> Option<crate::tile_pool::Slot> {
        self.pool.alloc()
    }

    /// Return a layer to the canvas pool.
    pub fn release(&mut self, slot: crate::tile_pool::Slot) {
        self.pool.free(slot);
    }

    /// Upload the CPU reference canvas's dirty tiles.
    ///
    /// The GPU is authoritative for painting (DECISIONS §4a), so in normal use there is
    /// nothing here: `openpaint_core::Canvas` holds no pixels unless the CPU reference
    /// path put them there. This is the seam that keeps that path usable.
    pub fn upload_dirty(&mut self, queue: &wgpu::Queue, canvas: &mut Canvas) {
        for coord in canvas.take_dirty() {
            let Some(tile) = canvas.tile(coord) else {
                continue;
            };
            let Some(layer) = self.ensure_tile_for_full_write(coord) else {
                continue;
            };
            debug_assert_eq!(tile.bytes().len(), TILE_BYTES);
            queue.write_texture(
                wgpu::ImageCopyTexture {
                    texture: self.pool.texture(),
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
        }
    }

    /// Write the camera and build the draw list for this frame.
    ///
    /// Culls to tiles that intersect the page: out-of-page tiles are retained storage, not
    /// visible content. The shader clamps too, so a stale instance can only ever draw
    /// nothing — belt and braces, because getting this wrong would silently show pixels a
    /// crop was supposed to hide.
    pub fn prepare(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, xform: PageToNdc) {
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

        let mut list: Vec<TileInstance> = self
            .map
            .iter()
            .filter(|(coord, _)| tile_intersects(*coord, self.page))
            .map(|(coord, slot)| TileInstance {
                coord: [coord.0, coord.1],
                layer: slot.layer(),
                _pad: 0,
            })
            .collect();
        // Deterministic order so a GPU capture is comparable between frames; the tiles are
        // disjoint and opaque, so order cannot affect the image.
        list.sort_unstable_by_key(|i| (i.coord[1], i.coord[0]));

        self.instance_count = list.len() as u32;
        if list.is_empty() {
            return;
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
}
