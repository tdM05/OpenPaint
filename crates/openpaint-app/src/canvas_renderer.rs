//! Renders an `openpaint_core::Canvas` to the window.
//!
//! Strategy for the Phase 0 fixed-size canvas: keep one GPU texture the size of
//! the whole canvas, initialized to the paper color. When tiles change, upload
//! just those tiles as sub-rectangles (`write_texture`) — so we still only
//! touch changed regions, preserving the point of the tile model. The texture
//! is drawn as a single quad, positioned by `crate::view::View`.
//!
//! ⚠️ **One whole-canvas texture is a known Phase-0 shortcut and contradicts a
//! decision we have already made.** DECISIONS §2 requires the GPU to hold a
//! *bounded* set of resident tiles, because an A4 page at 300 DPI is ~70 MB per
//! layer at `Rgba16Float` and a webtoon strip is unbounded — neither fits in a
//! Surface's shared graphics memory. At the current 2048² this costs ~34 MB and
//! works fine, so it is deliberately deferred to the GPU-rasterization work,
//! which needs to touch this file anyway. Tracked as OPEN_QUESTIONS Q13.

use half::f16;
use openpaint_core::tile::{TILE_BYTES, TILE_CHANNELS, TILE_SIZE};
use openpaint_core::Canvas;
use wgpu::util::DeviceExt;

use crate::view::Placement;

/// Uniform matching `Placement` in canvas.wgsl.
///
/// Four canvas corners in NDC, packed two per `vec4` so the uniform layout is
/// unambiguous (a bare `vec2` array in uniform space has a 16-byte stride, which
/// is an easy way to get silent corruption).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PlacementUniform {
    tl_tr: [f32; 4],
    bl_br: [f32; 4],
}

impl From<Placement> for PlacementUniform {
    fn from(p: Placement) -> Self {
        Self {
            tl_tr: [p.tl[0], p.tl[1], p.tr[0], p.tr[1]],
            bl_br: [p.bl[0], p.bl[1], p.br[0], p.br[1]],
        }
    }
}

pub struct CanvasRenderer {
    texture: wgpu::Texture,
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    placement_buf: wgpu::Buffer,
}

impl CanvasRenderer {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        surface_format: wgpu::TextureFormat,
        canvas: &Canvas,
    ) -> Self {
        let canvas_w = canvas.width();
        let canvas_h = canvas.height();

        // Canvas texture in linear premultiplied RGBA f16, matching the tile
        // format exactly (openpaint_core::tile) so uploads are a straight byte
        // copy. Linear is not optional: the surface is an *Srgb format, so wgpu
        // encodes on write, and handing it sRGB values would double-encode them.
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("canvas-texture"),
            size: wgpu::Extent3d {
                width: canvas_w,
                height: canvas_h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        // Initialize the whole texture to the paper color so unpainted area
        // reads as a clean sheet.
        let init = vec_paper(canvas_w, canvas_h, Canvas::paper_color());
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &init,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(canvas_w * texel_bytes()),
                rows_per_image: Some(canvas_h),
            },
            wgpu::Extent3d {
                width: canvas_w,
                height: canvas_h,
                depth_or_array_layers: 1,
            },
        );

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("canvas-sampler"),
            // Nearest when magnifying, so zooming in shows real pixels rather
            // than a blur -- what you want when inspecting brush edges. Linear
            // when minifying, so a zoomed-out canvas doesn't alias into noise.
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let placement_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("placement-uniform"),
            contents: bytemuck::bytes_of(&PlacementUniform {
                tl_tr: [-1.0, 1.0, 1.0, 1.0],
                bl_br: [-1.0, -1.0, 1.0, -1.0],
            }),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("canvas-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
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
                        view_dimension: wgpu::TextureViewDimension::D2,
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

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("canvas-bg"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: placement_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("canvas-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("canvas.wgsl").into()),
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("canvas-pl"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("canvas-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_main",
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        Self {
            texture,
            pipeline,
            bind_group,
            placement_buf,
        }
    }

    /// Upload the canvas's dirty tiles to the GPU texture.
    pub fn upload_dirty(&mut self, queue: &wgpu::Queue, canvas: &mut Canvas) {
        let dirty = canvas.take_dirty();
        for coord in dirty {
            let Some(tile) = canvas.tile(coord) else {
                continue;
            };
            // Tile origin in canvas pixels.
            let ox = coord.0 * TILE_SIZE as i32;
            let oy = coord.1 * TILE_SIZE as i32;
            // Tiles are always within the fixed canvas here, but guard anyway.
            if ox < 0 || oy < 0 {
                continue;
            }
            debug_assert_eq!(tile.bytes().len(), TILE_BYTES);
            queue.write_texture(
                wgpu::ImageCopyTexture {
                    texture: &self.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: ox as u32,
                        y: oy as u32,
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                tile.bytes(),
                wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(TILE_SIZE as u32 * texel_bytes()),
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

    /// Upload where the canvas quad should be drawn, as computed by
    /// [`crate::view::View`].
    ///
    /// The renderer deliberately does not compute this itself: the same transform
    /// has to map input back to canvas space, and keeping one owner is what stops
    /// the two drifting apart.
    pub fn set_placement(&self, queue: &wgpu::Queue, placement: Placement) {
        let uniform = PlacementUniform::from(placement);
        queue.write_buffer(&self.placement_buf, 0, bytemuck::bytes_of(&uniform));
    }

    /// Record draw commands into an existing render pass.
    pub fn draw<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.draw(0..6, 0..1);
    }
}

/// Bytes per texel of the canvas texture (RGBA f16).
fn texel_bytes() -> u32 {
    (TILE_CHANNELS * std::mem::size_of::<f16>()) as u32
}

/// Build an RGBA f16 buffer of `w*h` filled with a linear premultiplied color.
fn vec_paper(w: u32, h: u32, rgba_linear_premul: [f32; 4]) -> Vec<u8> {
    let texel: Vec<f16> = rgba_linear_premul
        .iter()
        .map(|c| f16::from_f32(*c))
        .collect();
    let row: Vec<f16> = texel.repeat((w * h) as usize);
    bytemuck::cast_slice(&row).to_vec()
}
