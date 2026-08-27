//! Renders an `openpaint_core::Canvas` to the window.
//!
//! Strategy for the Phase 0 fixed-size canvas: keep one GPU texture the size of
//! the whole canvas, initialized to the paper color. When tiles change, upload
//! just those tiles as sub-rectangles (`write_texture`) — so we still only
//! touch changed regions, preserving the point of the tile model. The texture
//! is drawn as a single quad, fitted and centered in the window.

use openpaint_core::tile::{TILE_BYTES, TILE_SIZE};
use openpaint_core::Canvas;
use wgpu::util::DeviceExt;

/// Uniform matching `Placement` in canvas.wgsl (std140: two vec2 -> 16 bytes).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Placement {
    min_ndc: [f32; 2],
    max_ndc: [f32; 2],
}

pub struct CanvasRenderer {
    canvas_w: u32,
    canvas_h: u32,
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

        // Canvas texture, non-sRGB storage; we sample and write straight bytes.
        // (Phase 0 blends in sRGB space; linear-correct compositing comes with
        // the real engine.)
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
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        // Initialize the whole texture to the paper color so unpainted area
        // reads as a clean sheet.
        let paper = canvas.paper_color();
        let init = vec_paper(canvas_w, canvas_h, paper);
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
                bytes_per_row: Some(canvas_w * 4),
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
            // Nearest keeps pixels crisp at 100%; we revisit filtering with zoom.
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let placement_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("placement-uniform"),
            contents: bytemuck::bytes_of(&Placement {
                min_ndc: [-1.0, 1.0],
                max_ndc: [1.0, -1.0],
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
            canvas_w,
            canvas_h,
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
            debug_assert_eq!(tile.pixels().len(), TILE_BYTES);
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
                tile.pixels(),
                wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(TILE_SIZE as u32 * 4),
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

    /// Recompute where the canvas quad sits: fit-and-center inside the surface
    /// with a small margin, preserving the canvas aspect ratio.
    pub fn update_placement(&self, queue: &wgpu::Queue, surface_w: u32, surface_h: u32) {
        let sw = surface_w.max(1) as f32;
        let sh = surface_h.max(1) as f32;
        let cw = self.canvas_w as f32;
        let ch = self.canvas_h as f32;

        let margin = 0.94; // leave a little breathing room around the canvas
        let scale = (sw / cw).min(sh / ch) * margin;
        let draw_w = cw * scale;
        let draw_h = ch * scale;

        // Half-extents in NDC.
        let hx = draw_w / sw;
        let hy = draw_h / sh;

        let placement = Placement {
            min_ndc: [-hx, hy],
            max_ndc: [hx, -hy],
        };
        queue.write_buffer(&self.placement_buf, 0, bytemuck::bytes_of(&placement));
    }

    /// Record draw commands into an existing render pass.
    pub fn draw<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.draw(0..6, 0..1);
    }

    /// Map a window pixel position to a canvas pixel position using the same
    /// fit-and-center math as `update_placement`. Returns `None` if the point
    /// falls outside the drawn canvas quad. Kept here so screen->canvas mapping
    /// can never drift from the placement used for drawing.
    pub fn screen_to_canvas(
        &self,
        px: f64,
        py: f64,
        surface_w: u32,
        surface_h: u32,
    ) -> Option<(f32, f32)> {
        let sw = surface_w.max(1) as f32;
        let sh = surface_h.max(1) as f32;
        let cw = self.canvas_w as f32;
        let ch = self.canvas_h as f32;

        let margin = 0.94;
        let scale = (sw / cw).min(sh / ch) * margin;
        let draw_w = cw * scale;
        let draw_h = ch * scale;

        // Quad is centered in the surface.
        let left = (sw - draw_w) * 0.5;
        let top = (sh - draw_h) * 0.5;

        let lx = px as f32 - left;
        let ly = py as f32 - top;
        if lx < 0.0 || ly < 0.0 || lx > draw_w || ly > draw_h {
            return None;
        }
        Some((lx / scale, ly / scale))
    }
}

/// Build an RGBA8 buffer of `w*h` filled with an opaque paper color.
fn vec_paper(w: u32, h: u32, paper: [u8; 3]) -> Vec<u8> {
    let rgba = [paper[0], paper[1], paper[2], 255];
    rgba.repeat((w * h) as usize)
}
