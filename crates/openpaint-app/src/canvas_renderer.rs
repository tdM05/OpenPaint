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

/// Pixel format of the canvas texture: linear premultiplied RGBA f16, matching
/// `openpaint_core::tile` exactly (DECISIONS §4b).
pub const CANVAS_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

pub struct CanvasRenderer {
    texture: wgpu::Texture,
    /// Kept so the stroke layer can bake into the canvas.
    view: wgpu::TextureView,
    /// Retained so the bind group can be rebuilt when the page is resized and the
    /// texture is replaced.
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
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
        let texture = create_canvas_texture(device, queue, canvas_w, canvas_h);

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
            view,
            bind_group_layout,
            sampler,
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

    /// Replace the canvas texture with a differently-sized one, copying existing
    /// content to `(dx, dy)`.
    ///
    /// One texture copy, which is why resizing is cheap on the GPU even though the
    /// CPU reference has to move pixels tile by tile: the shift rarely lands on a
    /// tile boundary, but a texture copy does not care.
    pub fn resize(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        new_w: u32,
        new_h: u32,
        dx: i32,
        dy: i32,
    ) {
        let old = std::mem::replace(
            &mut self.texture,
            create_canvas_texture(device, queue, new_w, new_h),
        );
        self.view = self
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        // The overlapping region, in both textures' coordinates. Clamped so a crop
        // (negative offset) copies only what actually lands inside.
        let old_size = old.size();
        let src_x = (-dx).max(0) as u32;
        let src_y = (-dy).max(0) as u32;
        let dst_x = dx.max(0) as u32;
        let dst_y = dy.max(0) as u32;
        let w = old_size
            .width
            .saturating_sub(src_x)
            .min(new_w.saturating_sub(dst_x));
        let h = old_size
            .height
            .saturating_sub(src_y)
            .min(new_h.saturating_sub(dst_y));

        if w > 0 && h > 0 {
            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("canvas-resize"),
            });
            encoder.copy_texture_to_texture(
                wgpu::ImageCopyTexture {
                    texture: &old,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: src_x,
                        y: src_y,
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::ImageCopyTexture {
                    texture: &self.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: dst_x,
                        y: dst_y,
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
            );
            queue.submit(std::iter::once(encoder.finish()));
        }

        // The bind group referenced the old view, so it has to be rebuilt.
        self.bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("canvas-bg"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.placement_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&self.view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
    }

    /// The canvas texture itself, for history's region copies.
    #[must_use]
    pub fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }

    /// The canvas texture as a render target, for the stroke layer to bake into.
    #[must_use]
    pub fn target_view(&self) -> &wgpu::TextureView {
        &self.view
    }

    /// Record draw commands into an existing render pass.
    pub fn draw<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.draw(0..6, 0..1);
    }
}

/// Create a canvas texture of the given size, filled with paper.
///
/// Shared by construction and resize so the two cannot drift in usage flags or
/// initial contents.
fn create_canvas_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    width: u32,
    height: u32,
) -> wgpu::Texture {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("canvas-texture"),
        size: wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: CANVAS_FORMAT,
        // RENDER_ATTACHMENT so the GPU stroke layer can bake into it
        // (crate::stroke_layer). COPY_SRC for history snapshots, export, and resize;
        // COPY_DST for the initial paper fill.
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_DST
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });

    let init = vec_paper(width.max(1), height.max(1), Canvas::paper_color());
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
            bytes_per_row: Some(width.max(1) * texel_bytes()),
            rows_per_image: Some(height.max(1)),
        },
        wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
    );
    texture
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_gpu::{readback, try_device, SIZE};

    /// Paint one texel of a known colour by uploading it directly.
    fn mark(queue: &wgpu::Queue, texture: &wgpu::Texture, x: u32, y: u32, value: [f32; 4]) {
        let texel: Vec<half::f16> = value.iter().map(|c| half::f16::from_f32(*c)).collect();
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x, y, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(&texel),
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(texel_bytes()),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
    }

    /// Extending down must leave every existing pixel exactly where it was, and the
    /// added region must be paper.
    ///
    /// This is where a resize can go silently wrong: swap the source and destination
    /// origins in the texture copy and content shifts to the wrong place with no
    /// error at all.
    #[test]
    fn resizing_down_preserves_content_and_papers_the_new_area() {
        let Some((device, queue)) = try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };

        let canvas = Canvas::new(SIZE, SIZE);
        let mut r = CanvasRenderer::new(
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8UnormSrgb,
            &canvas,
        );

        let red = [1.0, 0.0, 0.0, 1.0];
        mark(&queue, r.texture(), 7, 9, red);

        // Grow downward: content must not move.
        r.resize(&device, &queue, SIZE, SIZE * 2, 0, 0);

        let size = r.texture().size();
        assert_eq!((size.width, size.height), (SIZE, SIZE * 2));

        let pixels = read_full(&device, &queue, r.texture(), SIZE, SIZE * 2);
        let at = |x: u32, y: u32| pixels[(y * SIZE + x) as usize];
        assert!(
            at(7, 9)[0] > 0.9 && at(7, 9)[1] < 0.1,
            "mark lost: {:?}",
            at(7, 9)
        );

        // A pixel in the newly added region must be paper.
        let paper = Canvas::paper_color();
        let new_area = at(7, SIZE + 20);
        assert!(
            (new_area[0] - paper[0]).abs() < 0.01,
            "new area is not paper: {new_area:?}"
        );
    }

    /// Extending *upward* shifts content down by the growth. Getting this direction
    /// backwards would move content off the top instead.
    #[test]
    fn resizing_up_shifts_content_down() {
        let Some((device, queue)) = try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };

        let canvas = Canvas::new(SIZE, SIZE);
        let mut r = CanvasRenderer::new(
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8UnormSrgb,
            &canvas,
        );
        let red = [1.0, 0.0, 0.0, 1.0];
        mark(&queue, r.texture(), 7, 9, red);

        // Same growth, but anchored at the bottom, so dy = SIZE.
        r.resize(&device, &queue, SIZE, SIZE * 2, 0, SIZE as i32);

        let pixels = read_full(&device, &queue, r.texture(), SIZE, SIZE * 2);
        let at = |x: u32, y: u32| pixels[(y * SIZE + x) as usize];
        assert!(
            at(7, SIZE + 9)[0] > 0.9,
            "content did not follow the shift: {:?}",
            at(7, SIZE + 9)
        );
        let paper = Canvas::paper_color();
        assert!(
            (at(7, 9)[0] - paper[0]).abs() < 0.01,
            "old position should now be paper: {:?}",
            at(7, 9)
        );
    }

    /// A crop must keep the part that lands inside and not fail on the part that
    /// doesn't -- the copy extent has to be clamped, or wgpu rejects it.
    #[test]
    fn cropping_keeps_the_overlapping_region() {
        let Some((device, queue)) = try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };

        let canvas = Canvas::new(SIZE, SIZE);
        let mut r = CanvasRenderer::new(
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8UnormSrgb,
            &canvas,
        );
        mark(&queue, r.texture(), 5, 5, [1.0, 0.0, 0.0, 1.0]);

        let half = SIZE / 2;
        r.resize(&device, &queue, half, half, 0, 0);
        let size = r.texture().size();
        assert_eq!((size.width, size.height), (half, half));

        let pixels = read_full(&device, &queue, r.texture(), half, half);
        assert!(
            pixels[(5 * half + 5) as usize][0] > 0.9,
            "in-bounds mark lost"
        );
    }

    /// Read an arbitrary-sized canvas texture back, handling row padding.
    fn read_full(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        texture: &wgpu::Texture,
        w: u32,
        h: u32,
    ) -> Vec<[f32; 4]> {
        let unpadded = w * texel_bytes();
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded = unpadded.div_ceil(align) * align;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("resize-readback"),
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
            let halves: &[half::f16] = bytemuck::cast_slice(&mapped[start..end]);
            for c in halves.as_chunks::<4>().0 {
                out.push([c[0].to_f32(), c[1].to_f32(), c[2].to_f32(), c[3].to_f32()]);
            }
        }
        out
    }

    /// Silence the unused warning for readback, which is only used by other modules.
    #[allow(dead_code)]
    fn _uses_readback() {
        let _ = readback;
    }
}
