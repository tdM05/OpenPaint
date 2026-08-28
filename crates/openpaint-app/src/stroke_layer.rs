//! GPU dab rasterization — the fast path for DECISIONS §4a's per-pixel half.
//!
//! # Why this exists
//!
//! Dabs per stroke are in the hundreds; pixels per stroke are in the millions (see
//! `openpaint_core::dab`). A 400px dab is ~500k pixels, and the CPU reference does
//! one at a time with a hash lookup per pixel. This moves that work to the GPU.
//!
//! On the Surface-class target (DECISIONS §2) the *bandwidth* win matters as much
//! as the throughput one: the CPU path had to upload every changed tile across a
//! shared memory bus (512 KiB per tile per update), whereas this uploads the dab
//! list — a few hundred × 32 bytes.
//!
//! # The stroke is a separate layer until it ends
//!
//! Dabs accumulate into a single-channel texture, and the canvas is **not touched**
//! while the stroke is in progress. Mid-stroke the preview is drawn as a second
//! pass over the canvas; on stroke end the accumulated paint is baked into the
//! canvas once.
//!
//! That structure is what makes the flow/opacity model work without a snapshot:
//! *flow* accumulates per dab while *opacity* caps the stroke total, so the stroke
//! has to stay separable from what is underneath it right up until it's committed.
//! It also means no readback and no ping-pong target — the canvas is only ever
//! written by the blend unit.
//!
//! Compare `openpaint_core::stroke`, which does the same thing on the CPU and holds
//! a snapshot instead, because it *does* composite into the canvas each update.
//!
//! # Known limit
//!
//! The accumulation texture is canvas-sized, like the canvas texture itself. That
//! shares the whole-canvas shortcut recorded in OPEN_QUESTIONS Q13/Q15: fine at
//! 2048² (+8 MiB), wrong at 300 DPI. Both become tile-cache-sized together.

use openpaint_core::Dab;
use wgpu::util::DeviceExt;

use crate::view::Placement;

/// Per-dab instance data uploaded to the GPU.
///
/// Colour is deliberately absent: accumulation is scalar coverage, and the stroke
/// colour is applied once at composite time. Keeping colour out also means a
/// colour change mid-stroke can't produce a two-tone stroke.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DabInstance {
    center: [f32; 2],
    radius: f32,
    hardness: f32,
    flow: f32,
    _pad: [f32; 3],
}

impl From<&Dab> for DabInstance {
    fn from(d: &Dab) -> Self {
        Self {
            center: [d.x, d.y],
            radius: d.radius,
            hardness: d.hardness,
            flow: d.flow,
            _pad: [0.0; 3],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CanvasSizeUniform {
    size: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PaintUniform {
    color: [f32; 4],
    opacity: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PlacementUniform {
    tl_tr: [f32; 4],
    bl_br: [f32; 4],
}

/// Accumulation format. `R16Float` rather than `R8Unorm` because a stroke at low
/// flow takes many dabs to approach its ceiling, and 8 bits of headroom visibly
/// quantizes the build-up. Renderable and blendable everywhere WebGPU is.
pub const ACCUM_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R16Float;

pub struct StrokeLayer {
    accum_view: wgpu::TextureView,
    dab_pipeline: wgpu::RenderPipeline,
    /// Composites into the canvas texture (identity quad).
    bake_pipeline: wgpu::RenderPipeline,
    /// Composites onto the surface, following the on-screen canvas quad.
    preview_pipeline: wgpu::RenderPipeline,
    canvas_size_group: wgpu::BindGroup,
    paint_group: wgpu::BindGroup,
    placement_group: wgpu::BindGroup,
    paint_buf: wgpu::Buffer,
    placement_buf: wgpu::Buffer,
    /// Instance buffer, grown on demand and reused between updates.
    instances: wgpu::Buffer,
    instance_capacity: usize,
    /// Whether anything has been accumulated since the last clear.
    dirty: bool,
}

impl StrokeLayer {
    pub fn new(
        device: &wgpu::Device,
        canvas_w: u32,
        canvas_h: u32,
        canvas_format: wgpu::TextureFormat,
        surface_format: wgpu::TextureFormat,
    ) -> Self {
        let accum = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("stroke-accum"),
            size: wgpu::Extent3d {
                width: canvas_w.max(1),
                height: canvas_h.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: ACCUM_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let accum_view = accum.create_view(&wgpu::TextureViewDescriptor::default());

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("stroke-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("stroke.wgsl").into()),
        });

        // --- dab stamping -------------------------------------------------
        let canvas_size_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("canvas-size-uniform"),
            contents: bytemuck::bytes_of(&CanvasSizeUniform {
                size: [canvas_w as f32, canvas_h as f32, 0.0, 0.0],
            }),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let canvas_size_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("canvas-size-bgl"),
                entries: &[uniform_entry(0, wgpu::ShaderStages::VERTEX)],
            });
        let canvas_size_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("canvas-size-bg"),
            layout: &canvas_size_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: canvas_size_buf.as_entire_binding(),
            }],
        });

        let dab_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("dab-pl"),
            bind_group_layouts: &[&canvas_size_layout],
            push_constant_ranges: &[],
        });

        let dab_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("dab-pipeline"),
            layout: Some(&dab_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "dab_vs",
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<DabInstance>() as wgpu::BufferAddress,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &[
                        wgpu::VertexAttribute {
                            offset: 0,
                            shader_location: 0,
                            format: wgpu::VertexFormat::Float32x2,
                        },
                        wgpu::VertexAttribute {
                            offset: 8,
                            shader_location: 1,
                            format: wgpu::VertexFormat::Float32,
                        },
                        wgpu::VertexAttribute {
                            offset: 12,
                            shader_location: 2,
                            format: wgpu::VertexFormat::Float32,
                        },
                        wgpu::VertexAttribute {
                            offset: 16,
                            shader_location: 3,
                            format: wgpu::VertexFormat::Float32,
                        },
                    ],
                }],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "dab_fs",
                targets: &[Some(wgpu::ColorTargetState {
                    format: ACCUM_FORMAT,
                    // `a = src + dst * (1 - src)`, i.e. exactly the accumulation
                    // formula, computed by the blend unit. `OneMinusSrc` rather
                    // than `OneMinusSrcAlpha` because the target has only a red
                    // channel, so relying on .a would be needlessly subtle.
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrc,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrc,
                            operation: wgpu::BlendOperation::Add,
                        },
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        // --- compositing ---------------------------------------------------
        let paint_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("paint-uniform"),
            size: std::mem::size_of::<PaintUniform>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let placement_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("stroke-placement-uniform"),
            size: std::mem::size_of::<PlacementUniform>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("accum-sampler"),
            // Nearest: the accumulation texture is exactly canvas-resolution, so
            // any filtering would only smear the stroke against the canvas it is
            // composited over.
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let paint_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("paint-bgl"),
            entries: &[
                uniform_entry(0, wgpu::ShaderStages::FRAGMENT),
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                    count: None,
                },
            ],
        });
        let paint_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("paint-bg"),
            layout: &paint_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: paint_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&accum_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        let placement_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("stroke-placement-bgl"),
            entries: &[uniform_entry(0, wgpu::ShaderStages::VERTEX)],
        });
        let placement_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("stroke-placement-bg"),
            layout: &placement_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: placement_buf.as_entire_binding(),
            }],
        });

        let composite_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("composite-pl"),
            bind_group_layouts: &[&paint_layout, &placement_layout],
            push_constant_ranges: &[],
        });

        let make_composite = |label: &str, vs: &str, format: wgpu::TextureFormat| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&composite_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: vs,
                    buffers: &[],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: "paint_fs",
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        // Premultiplied "over": the fragment outputs premultiplied
                        // paint, so this needs no destination read in the shader.
                        blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
                cache: None,
            })
        };

        let bake_pipeline = make_composite("bake-pipeline", "paint_vs_identity", canvas_format);
        let preview_pipeline =
            make_composite("preview-pipeline", "paint_vs_placed", surface_format);

        let instance_capacity = 256;
        let instances = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("dab-instances"),
            size: (instance_capacity * std::mem::size_of::<DabInstance>()) as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            accum_view,
            dab_pipeline,
            bake_pipeline,
            preview_pipeline,
            canvas_size_group,
            paint_group,
            placement_group,
            paint_buf,
            placement_buf,
            instances,
            instance_capacity,
            dirty: false,
        }
    }

    /// Whether the layer holds any paint that still needs showing or baking.
    #[must_use]
    pub fn has_paint(&self) -> bool {
        self.dirty
    }

    /// Clear the accumulation texture, starting a fresh stroke.
    ///
    /// A load-op clear rather than a compute or copy: it is the cheapest way to
    /// zero a render target, and stroke starts are frequent.
    pub fn begin_stroke(&mut self, encoder: &mut wgpu::CommandEncoder) {
        encoder
            .begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("stroke-clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.accum_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            })
            .forget_lifetime();
        self.dirty = false;
    }

    /// Upload a whole frame's dabs in one go.
    ///
    /// Deliberately separate from stamping, and this separation is load-bearing:
    /// `Queue::write_buffer` does **not** interleave with recorded draws. Every
    /// write in a submission is applied *before* any command buffer in it executes,
    /// so writing the same buffer once per batch and drawing between writes means
    /// all the draws see only the last batch's data. That produced visible gaps in
    /// fast strokes, where several input batches land in one frame.
    ///
    /// Uploading once and drawing sub-ranges avoids the hazard entirely, and suits
    /// the data anyway: the editor already hands over a flat dab buffer with its
    /// commands indexing into it.
    pub fn upload_dabs(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, dabs: &[Dab]) {
        if dabs.is_empty() {
            return;
        }
        let data: Vec<DabInstance> = dabs.iter().map(DabInstance::from).collect();

        if data.len() > self.instance_capacity {
            // Grow generously; a long stroke segment can produce a lot of dabs at
            // once and reallocating per update would be wasteful.
            self.instance_capacity = data.len().next_power_of_two();
            self.instances = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("dab-instances"),
                size: (self.instance_capacity * std::mem::size_of::<DabInstance>())
                    as wgpu::BufferAddress,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        queue.write_buffer(&self.instances, 0, bytemuck::cast_slice(&data));
    }

    /// Stamp dabs `[start, start + len)` of the uploaded buffer into the
    /// accumulation texture.
    pub fn stamp_range(&mut self, encoder: &mut wgpu::CommandEncoder, start: usize, len: usize) {
        if len == 0 {
            return;
        }
        let mut pass = encoder
            .begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("dab-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.accum_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // Load, not clear: accumulation is the whole point.
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            })
            .forget_lifetime();

        pass.set_pipeline(&self.dab_pipeline);
        pass.set_bind_group(0, &self.canvas_size_group, &[]);
        pass.set_vertex_buffer(0, self.instances.slice(..));
        pass.draw(0..6, start as u32..(start + len) as u32);
        drop(pass);

        self.dirty = true;
    }

    /// Update the stroke colour and opacity used when compositing.
    pub fn set_paint(&self, queue: &wgpu::Queue, color_linear_premul: [f32; 4], opacity: f32) {
        queue.write_buffer(
            &self.paint_buf,
            0,
            bytemuck::bytes_of(&PaintUniform {
                color: color_linear_premul,
                opacity: [opacity.clamp(0.0, 1.0), 0.0, 0.0, 0.0],
            }),
        );
    }

    /// Composite the stroke onto the surface, following the canvas quad.
    ///
    /// Used mid-stroke so the canvas underneath stays untouched.
    pub fn draw_preview(
        &self,
        queue: &wgpu::Queue,
        pass: &mut wgpu::RenderPass<'_>,
        placement: Placement,
    ) {
        queue.write_buffer(
            &self.placement_buf,
            0,
            bytemuck::bytes_of(&PlacementUniform {
                tl_tr: [
                    placement.tl[0],
                    placement.tl[1],
                    placement.tr[0],
                    placement.tr[1],
                ],
                bl_br: [
                    placement.bl[0],
                    placement.bl[1],
                    placement.br[0],
                    placement.br[1],
                ],
            }),
        );
        pass.set_pipeline(&self.preview_pipeline);
        pass.set_bind_group(0, &self.paint_group, &[]);
        pass.set_bind_group(1, &self.placement_group, &[]);
        pass.draw(0..6, 0..1);
    }

    /// Bake the accumulated stroke into the canvas texture, committing it.
    pub fn bake(&mut self, encoder: &mut wgpu::CommandEncoder, canvas_view: &wgpu::TextureView) {
        if !self.dirty {
            return;
        }
        let mut pass = encoder
            .begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("stroke-bake"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: canvas_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            })
            .forget_lifetime();
        pass.set_pipeline(&self.bake_pipeline);
        pass.set_bind_group(0, &self.paint_group, &[]);
        pass.set_bind_group(1, &self.placement_group, &[]);
        pass.draw(0..6, 0..1);
        drop(pass);

        self.dirty = false;
    }
}

fn uniform_entry(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openpaint_core::{Canvas, StrokePainter};

    const SIZE: u32 = 128;
    const BLACK: [f32; 4] = [0.0, 0.0, 0.0, 1.0];

    /// A headless device, or `None` where there is no usable adapter (some CI
    /// runners). Skipping is deliberate: the test is worth having where it can run,
    /// and a hard failure on a machine with no GPU would say nothing about the code.
    fn try_device() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))?;
        pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("test-device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::default(),
            },
            None,
        ))
        .ok()
    }

    fn dab(x: f32, y: f32, radius: f32, hardness: f32, flow: f32) -> Dab {
        Dab {
            x,
            y,
            radius,
            hardness,
            flow,
            color_linear_premul: BLACK,
        }
    }

    /// Rasterize `dabs` on the GPU and read the canvas back as linear f32 RGBA.
    fn gpu_render(dabs: &[Dab], opacity: f32) -> Vec<[f32; 4]> {
        gpu_render_batched(dabs, opacity, 1)
    }

    /// As `gpu_render`, but stamping the dabs in `batches` separate draw calls
    /// within one frame -- which is what a fast stroke produces.
    fn gpu_render_batched(dabs: &[Dab], opacity: f32, batches: usize) -> Vec<[f32; 4]> {
        let (device, queue) = try_device().expect("checked by caller");

        let canvas_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("test-canvas"),
            size: wgpu::Extent3d {
                width: SIZE,
                height: SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: crate::canvas_renderer::CANVAS_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });

        // Start from paper, exactly as the real canvas texture does.
        let paper = Canvas::paper_color();
        let texel: Vec<half::f16> = paper.iter().map(|c| half::f16::from_f32(*c)).collect();
        let filled: Vec<half::f16> = texel.repeat((SIZE * SIZE) as usize);
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &canvas_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(&filled),
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(SIZE * 8),
                rows_per_image: Some(SIZE),
            },
            wgpu::Extent3d {
                width: SIZE,
                height: SIZE,
                depth_or_array_layers: 1,
            },
        );
        let canvas_view = canvas_tex.create_view(&wgpu::TextureViewDescriptor::default());

        // The preview pipeline's format is irrelevant here; it is never drawn.
        let mut layer = StrokeLayer::new(
            &device,
            SIZE,
            SIZE,
            crate::canvas_renderer::CANVAS_FORMAT,
            wgpu::TextureFormat::Rgba8UnormSrgb,
        );

        let mut encoder = device.create_command_encoder(&Default::default());
        layer.begin_stroke(&mut encoder);
        layer.set_paint(&queue, BLACK, opacity);
        layer.upload_dabs(&device, &queue, dabs);
        // Split into `batches` draw calls, mimicking several input batches landing
        // in a single frame.
        let per = dabs.len().div_ceil(batches.max(1));
        let mut start = 0;
        while start < dabs.len() {
            let len = per.min(dabs.len() - start);
            layer.stamp_range(&mut encoder, start, len);
            start += len;
        }
        layer.bake(&mut encoder, &canvas_view);

        let bytes = (SIZE * SIZE * 8) as wgpu::BufferAddress;
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: bytes,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture: &canvas_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyBuffer {
                buffer: &readback,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(SIZE * 8),
                    rows_per_image: Some(SIZE),
                },
            },
            wgpu::Extent3d {
                width: SIZE,
                height: SIZE,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(std::iter::once(encoder.finish()));

        readback.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        device.poll(wgpu::Maintain::Wait);
        let view = readback.slice(..).get_mapped_range();
        let halves: &[half::f16] = bytemuck::cast_slice(&view);
        halves
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| [c[0].to_f32(), c[1].to_f32(), c[2].to_f32(), c[3].to_f32()])
            .collect()
    }

    /// Rasterize the same dabs through the CPU reference implementation.
    fn cpu_render(dabs: &[Dab], opacity: f32) -> Vec<[f32; 4]> {
        let mut canvas = Canvas::new(SIZE, SIZE);
        let mut painter = StrokePainter::new();
        painter.begin();
        painter.add_dabs(&canvas, dabs);
        painter.composite(&mut canvas, BLACK, opacity);

        let paper = Canvas::paper_color().map(|c| half::f16::from_f32(c).to_f32());
        (0..SIZE)
            .flat_map(|y| (0..SIZE).map(move |x| (x, y)))
            .map(|(x, y)| match canvas.tile((0, 0)) {
                // Tiles are 256 wide, so the whole 128x128 canvas is tile (0, 0).
                Some(tile) => tile.texel(x as usize, y as usize),
                None => paper,
            })
            .collect()
    }

    /// Compare the two, returning the largest per-channel difference.
    fn max_difference(a: &[[f32; 4]], b: &[[f32; 4]]) -> (f32, usize) {
        let mut worst = 0.0;
        let mut worst_at = 0;
        for (i, (p, q)) in a.iter().zip(b).enumerate() {
            for c in 0..4 {
                let d = (p[c] - q[c]).abs();
                if d > worst {
                    worst = d;
                    worst_at = i;
                }
            }
        }
        (worst, worst_at)
    }

    /// Mean absolute difference across all channels.
    fn mean_difference(a: &[[f32; 4]], b: &[[f32; 4]]) -> f32 {
        let mut total = 0.0f64;
        for (p, q) in a.iter().zip(b) {
            for c in 0..4 {
                total += f64::from((p[c] - q[c]).abs());
            }
        }
        (total / (a.len() * 4) as f64) as f32
    }

    fn any_paint(pixels: &[[f32; 4]]) -> bool {
        let paper = Canvas::paper_color();
        pixels.iter().any(|p| (p[0] - paper[0]).abs() > 0.05)
    }

    /// The reason `openpaint_core::raster` and `openpaint_core::stroke` are kept:
    /// the falloff curve exists twice, once in `Dab::coverage_at_distance` and once
    /// in `stroke.wgsl`'s `dab_fs`. Two copies of a curve drift. This is what
    /// notices.
    #[test]
    fn gpu_rasterization_matches_the_cpu_reference() {
        let Some(_) = try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };

        // Opaque dabs at a few hardnesses, overlapping so compositing order matters.
        let dabs = [
            dab(40.0, 64.0, 18.0, 1.0, 1.0),
            dab(56.0, 64.0, 18.0, 0.5, 1.0),
            dab(72.0, 64.0, 18.0, 0.0, 1.0),
            dab(88.0, 70.0, 12.0, 0.25, 1.0),
        ];

        let gpu = gpu_render(&dabs, 1.0);
        let cpu = cpu_render(&dabs, 1.0);

        assert!(any_paint(&gpu), "GPU produced no paint at all");
        assert!(any_paint(&cpu), "CPU reference produced no paint");

        let (worst, at) = max_difference(&gpu, &cpu);
        assert!(
            worst < 0.01,
            "GPU and CPU disagree by {worst} at pixel {at} ({}, {}); \
             gpu {:?} cpu {:?}",
            at as u32 % SIZE,
            at as u32 / SIZE,
            gpu[at],
            cpu[at]
        );
    }

    /// Build-up with a low flow and an opacity ceiling.
    ///
    /// Uses *soft* dabs deliberately. With `hardness = 1.0` coverage is a step
    /// function, so a sub-pixel sampling difference between the two
    /// implementations flips a whole dab on or off at the rim -- and with several
    /// overlapping dabs that is a ~12% swing in the accumulated value. That is
    /// inherent to comparing hard edges, not a defect, so the strict comparison
    /// uses a continuous falloff where a small position error can only produce a
    /// small value error.
    #[test]
    fn gpu_build_up_matches_the_cpu_reference() {
        let Some(_) = try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };

        // Densely overlapping soft dabs, so coverage accumulates many times over.
        let dabs: Vec<Dab> = (0..40)
            .map(|i| dab(50.0 + i as f32 * 0.7, 64.0, 14.0, 0.5, 0.1))
            .collect();

        let opacity = 0.5;
        let gpu = gpu_render(&dabs, opacity);
        let cpu = cpu_render(&dabs, opacity);

        assert!(any_paint(&gpu), "GPU produced no paint at all");

        let (worst, at) = max_difference(&gpu, &cpu);
        assert!(
            worst < 0.02,
            "build-up diverged by {worst} at pixel ({}, {}); gpu {:?} cpu {:?}",
            at as u32 % SIZE,
            at as u32 / SIZE,
            gpu[at],
            cpu[at]
        );

        // Max difference can be dominated by a handful of rim pixels, so also
        // require the two to agree closely on average.
        let mean = mean_difference(&gpu, &cpu);
        assert!(mean < 0.002, "mean difference {mean} is too large");

        // And the ceiling must actually bind: black at opacity 0.5 over paper can
        // never darken past halfway.
        let paper = Canvas::paper_color()[0];
        let darkest = gpu.iter().map(|p| p[0]).fold(f32::MAX, f32::min);
        assert!(
            darkest > paper * (1.0 - opacity) - 0.02,
            "GPU passed the opacity ceiling: darkest {darkest}, floor {}",
            paper * (1.0 - opacity)
        );
    }

    /// Regression: a fast stroke delivers several input batches in one frame, so the
    /// dabs are stamped in several draw calls. Splitting them must be
    /// indistinguishable from one call.
    ///
    /// This failed before `upload_dabs` was separated from stamping: writing the
    /// instance buffer once per batch looks correct, but `Queue::write_buffer`
    /// applies every write in a submission *before* any of its command buffers run,
    /// so all the draws saw only the final batch. It showed up as visible gaps in
    /// fast strokes -- worse the faster the stroke, because more batches landed per
    /// frame.
    #[test]
    fn batching_dabs_does_not_change_the_result() {
        let Some(_) = try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };

        // A long run of overlapping dabs, like a fast stroke across the canvas.
        let dabs: Vec<Dab> = (0..48)
            .map(|i| dab(20.0 + i as f32 * 1.8, 64.0, 9.0, 0.5, 0.6))
            .collect();

        let one_call = gpu_render_batched(&dabs, 1.0, 1);
        assert!(any_paint(&one_call), "produced no paint at all");

        for batches in [2, 5, 12] {
            let split = gpu_render_batched(&dabs, 1.0, batches);
            let (worst, at) = max_difference(&one_call, &split);
            assert!(
                worst < 0.01,
                "{batches} batches differ from 1 by {worst} at ({}, {}); \
                 one {:?} split {:?}",
                at as u32 % SIZE,
                at as u32 / SIZE,
                one_call[at],
                split[at]
            );
        }
    }
}
