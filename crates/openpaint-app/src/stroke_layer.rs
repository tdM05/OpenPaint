//! GPU dab rasterization — the fast path for DECISIONS §4a's per-pixel half.
//!
//! # Why this exists
//!
//! Dabs per stroke are in the hundreds; pixels per stroke are in the millions (see
//! `openpaint_core::dab`). A 400px dab is ~500k pixels, and the CPU reference does one at
//! a time with a hash lookup per pixel. This moves that work to the GPU.
//!
//! On the Surface-class target (DECISIONS §2) the *bandwidth* win matters as much as the
//! throughput one: the CPU path had to upload every changed tile across a shared memory
//! bus (512 KiB per tile per update), whereas this uploads the dab list — a few hundred ×
//! 32 bytes.
//!
//! # The stroke is a separate layer until it ends
//!
//! Dabs accumulate into a single-channel buffer, and the canvas is **not touched** while the
//! stroke is in progress. On stroke end the accumulated paint is baked into the active layer
//! once.
//!
//! Mid-stroke, the *compositor* reads this buffer and injects it into the active layer as it
//! walks the stack (DECISIONS §4e), so there is no preview pass here. That is why the preview
//! and the committed result cannot disagree: they are the same arithmetic, run on the same
//! accumulation, in the same shader.
//!
//! That structure is what makes the flow/opacity model work without a snapshot: *flow*
//! accumulates per dab while *opacity* caps the stroke total, so the stroke has to stay
//! separable from what is underneath it right up until it's committed. It also means no
//! readback and no ping-pong target — the canvas is only ever written by the blend unit.
//!
//! # Accumulation is tiled, for the same reason the canvas is
//!
//! Because opacity caps the stroke *total*, the accumulation buffer has to cover
//! everywhere the stroke has been — potentially the whole canvas. A page-sized
//! accumulation texture would put back exactly the ceiling the tiled canvas removed, so
//! this keeps its own sparse pool: one layer per tile the stroke has touched, allocated on
//! demand and released when the stroke commits.
//!
//! Compare `openpaint_core::stroke`, which does the same thing on the CPU and holds a
//! snapshot instead, because it *does* composite into the canvas each update.

use std::collections::HashSet;

use openpaint_core::tile::{TileCoord, TILE_SIZE};
use openpaint_core::{Dab, PageRect};

use crate::canvas_renderer::{tile_intersects, tile_of, CanvasRenderer};
use crate::tile_pool::{layers_for_budget, TileMap, TilePool};
use crate::tile_store::LayerId;
use crate::view::PageToNdc;

/// Accumulation format. `R16Float` rather than `R8Unorm` because a stroke at low flow
/// takes many dabs to approach its ceiling, and 8 bits of headroom visibly quantizes the
/// build-up. Renderable and blendable everywhere WebGPU is.
pub const ACCUM_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R16Float;

/// Bytes per accumulation texel (one f16).
const ACCUM_BYTES_PER_TEXEL: u32 = 2;

/// GPU memory the in-progress stroke's accumulation tiles may occupy.
///
/// 32 MiB is 256 tiles at 128 KiB each — a stroke covering 16.7 Mpx, which is a stroke
/// across a fully-inked A4 page at 300 DPI twice over. A single stroke bigger than that is
/// pathological rather than normal use, so it is reported rather than designed for.
const ACCUM_BUDGET_BYTES: u64 = 32 * 1024 * 1024;

/// Stride between per-tile uniform records.
///
/// Dynamic uniform offsets must be a multiple of `min_uniform_buffer_offset_alignment`,
/// which is 256 under the default limits this app deliberately requests (DECISIONS §2).
const TILE_PARAMS_STRIDE: u64 = 256;

/// The per-tile uniform buffer is split into two disjoint regions: records for this
/// frame's stamping, and records for a bake.
///
/// This is not tidiness. A bake happens in the same submission as the stamps that preceded
/// it, and `Queue::write_buffer` applies *every* write in a submission before *any* command
/// buffer runs -- so publishing the bake's records over the stamps' records would leave the
/// stamps reading the bake's tiles. Separate regions make that impossible rather than
/// merely avoided, which is the right way to handle a hazard that has already bitten this
/// app once (see `upload_dabs`).
#[derive(Clone, Copy)]
enum Region {
    Stamp,
    Bake,
}

/// Per-dab instance data uploaded to the GPU.
///
/// Colour is deliberately absent: accumulation is scalar coverage, and the stroke colour is
/// applied once at composite time. Keeping colour out also means a colour change mid-stroke
/// can't produce a two-tone stroke.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DabInstance {
    center: [f32; 2],
    radius: f32,
    hardness: f32,
    flow: f32,
    roundness: f32,
    angle: f32,
    _pad: f32,
}

impl From<&Dab> for DabInstance {
    fn from(d: &Dab) -> Self {
        Self {
            center: [d.x, d.y],
            radius: d.radius,
            hardness: d.hardness,
            flow: d.flow,
            roundness: d.roundness,
            angle: d.angle,
            _pad: 0.0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PaintUniform {
    color: [f32; 4],
    opacity: [f32; 4],
}

/// Upload a brush tip as an `R8Unorm` texture, or a 1x1 placeholder when there is none.
///
/// A placeholder rather than an unbound slot: wgpu has no null resource, and making the binding
/// optional would mean a second pipeline and a second layout for a branch the shader already takes
/// for free.
fn upload_stamp(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    stamp: &openpaint_core::Stamp,
) -> wgpu::TextureView {
    let (width, height) = (stamp.width(), stamp.height());
    let data = stamp.coverage();
    let texture = stamp_texture(device, width, height);
    // `write_texture` requires rows padded to 256 bytes, and a tip is rarely a multiple of that,
    // so narrow tips are padded here rather than being silently skewed.
    let padded = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let row = width;
    let padded_row = row.div_ceil(padded) * padded;
    let mut staged = vec![0_u8; (padded_row * height) as usize];
    for y in 0..height {
        let src = (y * row) as usize;
        let dst = (y * padded_row) as usize;
        staged[dst..dst + row as usize].copy_from_slice(&data[src..src + row as usize]);
    }
    queue.write_texture(
        wgpu::ImageCopyTexture {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &staged,
        wgpu::ImageDataLayout {
            offset: 0,
            bytes_per_row: Some(padded_row),
            rows_per_image: Some(height),
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

/// The texture a brush tip lives in: one channel, because a tip is coverage and not colour.
fn stamp_texture(device: &wgpu::Device, width: u32, height: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("brush-tip"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

/// A 1x1 stand-in bound while the tip is a round brush.
///
/// Bound rather than left empty because wgpu has no null resource, and making the binding optional
/// would mean a second pipeline and a second layout for a branch the shader already takes for free.
/// Left unwritten: wgpu zero-initializes, and the shader only samples this slot when the tip is a
/// stamp — at which point a real texture has been bound alongside the flag that says so, in the one
/// function that sets either.
fn placeholder_stamp(device: &wgpu::Device) -> wgpu::TextureView {
    stamp_texture(device, 1, 1).create_view(&wgpu::TextureViewDescriptor::default())
}

/// Build the group holding the camera uniform and the bound tip.
///
/// Rebuilt rather than mutated when the tip changes: a bind group is immutable once created.
fn make_xform_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    xform_buf: &wgpu::Buffer,
    stamp: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("stroke-xform-bg"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: xform_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(stamp),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    })
}

/// Matching `TileParams` in stroke.wgsl.
///
/// Padding up to `TILE_PARAMS_STRIDE` is added when records are packed, not carried as a
/// field: a 224-byte array in the struct would be dead weight in every copy and would have
/// to be kept in step with the stride by hand.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TileParamsRecord {
    tile: [f32; 4],
    layer: [u32; 4],
}

/// Matching `Xform` in stroke.wgsl.
/// Sample a curve into the shader's lookup table.
fn sample_falloff(curve: &openpaint_core::Curve) -> [[f32; 4]; 8] {
    let mut out = [[0.0_f32; 4]; 8];
    for i in 0..openpaint_core::dab::FALLOFF_SAMPLES {
        #[allow(clippy::cast_precision_loss)]
        let t = i as f32 / (openpaint_core::dab::FALLOFF_SAMPLES - 1) as f32;
        out[i / 4][i % 4] = curve.at(t);
    }
    out
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct XformUniform {
    x_row: [f32; 4],
    y_row: [f32; 4],
    page: [f32; 4],
    params: [f32; 4],
    /// The edge profile as a lookup table, in `vec4`-sized rows because that is what a uniform
    /// array must be aligned to.
    falloff: [[f32; 4]; 8],
}

pub struct StrokeLayer {
    pool: TilePool,
    /// Accumulation tiles for the stroke in progress.
    map: TileMap,
    /// The current stroke's edge profile, already sampled for the shader.
    ///
    /// Held here rather than recomputed per frame because it changes only when the brush does, and
    /// sampling a spline thirty-two times per frame to produce the same numbers would be work for
    /// nothing.
    falloff: [[f32; 4]; 8],
    /// Tiles in this frame's stamping records, in record order.
    stamp_records: Vec<TileCoord>,
    dab_pipeline: wgpu::RenderPipeline,
    bake_pipeline: wgpu::RenderPipeline,
    /// Same geometry and the same accumulation; only the blend differs.
    erase_pipeline: wgpu::RenderPipeline,
    lock_pipeline: wgpu::RenderPipeline,
    paint_group: wgpu::BindGroup,
    /// Bound at group 0 during the dab pass, where the accumulation array is the render
    /// target and must not also be sampled.
    empty_group: wgpu::BindGroup,
    tile_group: wgpu::BindGroup,
    xform_group: wgpu::BindGroup,
    /// Kept so the group can be rebuilt when the tip changes.
    xform_layout: wgpu::BindGroupLayout,
    stamp_sampler: wgpu::Sampler,
    /// Whether the bound tip is a bitmap, which is what tells the shader which branch to take.
    stamped: bool,
    paint_buf: wgpu::Buffer,
    tile_buf: wgpu::Buffer,
    xform_buf: wgpu::Buffer,
    /// Dab instances, grown on demand and reused between updates.
    instances: wgpu::Buffer,
    instance_capacity: usize,
    /// Paint captured at stroke start, which the compositor needs to show the preview.
    paint: ([f32; 4], f32),
    /// Whether the stroke in progress removes paint rather than adding it.
    mode: crate::editor::PaintMode,
    /// The camera and page last published, so the page can be refreshed without the
    /// caller having to supply a camera it does not have.
    frame: (PageToNdc, PageRect),
    /// Whether anything has been accumulated since the last clear.
    dirty: bool,
    /// Set when a stroke could not allocate an accumulation tile, so the caller can say
    /// so rather than silently dropping paint.
    exhausted: bool,
}

impl StrokeLayer {
    pub fn new(device: &wgpu::Device, canvas_format: wgpu::TextureFormat) -> Self {
        let capacity = layers_for_budget(device, ACCUM_BYTES_PER_TEXEL, ACCUM_BUDGET_BYTES);
        let pool = TilePool::new(
            device,
            "stroke-accum",
            ACCUM_FORMAT,
            capacity,
            ACCUM_BYTES_PER_TEXEL,
            // COPY_DST because a fill writes coverage straight into these tiles rather than
            // rendering dabs into them -- the one path that produces accumulation without a
            // render pass.
            wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST,
        );

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("stroke-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("stroke.wgsl").into()),
        });

        let paint_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("paint-uniform"),
            size: std::mem::size_of::<PaintUniform>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let tile_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("stroke-tile-params"),
            // Two disjoint regions -- see `Region`.
            size: TILE_PARAMS_STRIDE * u64::from(capacity) * 2,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let xform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("stroke-xform"),
            size: std::mem::size_of::<XformUniform>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("accum-sampler"),
            // Nearest: accumulation is exactly canvas-resolution, so any filtering would
            // only smear the stroke against the canvas it is composited over.
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let paint_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("paint-bgl"),
            entries: &[
                uniform_entry(0, wgpu::ShaderStages::FRAGMENT, false),
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
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
                    resource: wgpu::BindingResource::TextureView(pool.array_view()),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        let tile_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("stroke-tile-bgl"),
            entries: &[uniform_entry(
                0,
                wgpu::ShaderStages::VERTEX_FRAGMENT,
                // Dynamic, so one write per frame can serve every tile's pass. See the
                // note on `TileParams` in stroke.wgsl.
                true,
            )],
        });
        let tile_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("stroke-tile-bg"),
            layout: &tile_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &tile_buf,
                    offset: 0,
                    size: std::num::NonZeroU64::new(32),
                }),
            }],
        });

        let xform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("stroke-xform-bgl"),
            entries: &[
                // Both stages: the vertex shader places the quad, and the fragment shader reads
                // the edge profile out of the same uniform.
                uniform_entry(0, wgpu::ShaderStages::VERTEX_FRAGMENT, false),
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
        // A round brush still binds a texture -- a 1x1 -- rather than leaving the slot empty. An
        // optional binding would mean a second pipeline and a second layout for a branch that
        // costs nothing taken, and wgpu has no null resource anyway.
        let stamp_view = placeholder_stamp(device);
        let stamp_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("stroke-stamp-sampler"),
            // Clamped, not repeated: wrapping would put the far side of a tip against its near
            // side, which reads as a seam around every dab. Linear, and `Stamp::sample` mirrors it
            // exactly so the cross-check can hold the two together.
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let xform_group = make_xform_group(
            device,
            &xform_layout,
            &xform_buf,
            &stamp_view,
            &stamp_sampler,
        );

        // The dab pass renders *into* the accumulation array, so it must not also have it
        // bound as a sampled texture -- a colour target is an exclusive usage within a pass,
        // and binding both is a validation error rather than merely wasteful.
        //
        // WGSL fixes a binding's group in the module, so groups cannot be renumbered per
        // entry point. Instead the dab pipeline gets an **empty** layout at group 0: the
        // slot still exists positionally, and nothing is bound in it.
        let empty_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("stroke-empty-bgl"),
            entries: &[],
        });
        let empty_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("stroke-empty-bg"),
            layout: &empty_layout,
            entries: &[],
        });

        let dab_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("dab-pl"),
            bind_group_layouts: &[&empty_layout, &tile_layout, &xform_layout],
            push_constant_ranges: &[],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("stroke-pl"),
            bind_group_layouts: &[&paint_layout, &tile_layout, &xform_layout],
            push_constant_ranges: &[],
        });

        let dab_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("dab-pipeline"),
            layout: Some(&dab_pipeline_layout),
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
                        wgpu::VertexAttribute {
                            offset: 20,
                            shader_location: 4,
                            format: wgpu::VertexFormat::Float32,
                        },
                        wgpu::VertexAttribute {
                            offset: 24,
                            shader_location: 5,
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
                    // `a = src + dst * (1 - src)`, i.e. exactly the accumulation formula,
                    // computed by the blend unit. `OneMinusSrc` rather than
                    // `OneMinusSrcAlpha` because the target has only a red channel, so
                    // relying on .a would be needlessly subtle.
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

        let composite = |label: &str,
                         vs: &str,
                         format: wgpu::TextureFormat,
                         buffers: &[wgpu::VertexBufferLayout],
                         blend: wgpu::BlendState| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: vs,
                    buffers,
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: "paint_fs",
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(blend),
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

        // Premultiplied "over": the fragment outputs premultiplied paint, so this needs no
        // destination read in the shader.
        let bake_pipeline = composite(
            "bake-pipeline",
            "bake_vs",
            canvas_format,
            &[],
            wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING,
        );
        // Erasing is `dst * (1 - coverage)`, which the blend unit computes with a zero source
        // factor -- so the *same* fragment shader serves both, and an eraser cannot drift from
        // the brush in shape, falloff or spacing. It scales the premultiplied colour along with
        // the alpha, which is exactly right: premultiplied means coverage already lives in every
        // channel.
        let erase_blend = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::Zero,
            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
            operation: wgpu::BlendOperation::Add,
        };
        let erase_pipeline = composite(
            "erase-pipeline",
            "bake_vs",
            canvas_format,
            &[],
            wgpu::BlendState {
                color: erase_blend,
                alpha: erase_blend,
            },
        );

        // Alpha lock is source-atop: `src * dst.a + dst * (1 - src.a)`. For the alpha channel that
        // reduces to `dst.a` exactly -- `src.a * dst.a + dst.a * (1 - src.a) == dst.a` -- so
        // coverage cannot change however hard you scrub, without a mask, a read of the target, or a
        // second shader. A third blend state, and again the same fragment shader, so a locked stroke
        // cannot drift from an unlocked one in shape, falloff or spacing.
        let lock_blend = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::DstAlpha,
            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
            operation: wgpu::BlendOperation::Add,
        };
        let lock_pipeline = composite(
            "lock-alpha-pipeline",
            "bake_vs",
            canvas_format,
            &[],
            wgpu::BlendState {
                color: lock_blend,
                alpha: lock_blend,
            },
        );

        let instance_capacity = 256;
        let instances = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("dab-instances"),
            size: (instance_capacity * std::mem::size_of::<DabInstance>()) as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            pool,
            map: TileMap::default(),
            stamp_records: Vec::new(),
            dab_pipeline,
            bake_pipeline,
            erase_pipeline,
            lock_pipeline,
            paint_group,
            empty_group,
            tile_group,
            xform_group,
            xform_layout,
            stamp_sampler,
            stamped: false,
            paint_buf,
            tile_buf,
            xform_buf,
            instances,
            instance_capacity,
            paint: ([0.0; 4], 1.0),
            mode: crate::editor::PaintMode::Normal,
            falloff: sample_falloff(&openpaint_core::dab::linear_falloff()),
            frame: (
                PageToNdc {
                    x_row: [0.0; 3],
                    y_row: [0.0; 3],
                },
                PageRect::from_size(1, 1),
            ),
            dirty: false,
            exhausted: false,
        }
    }

    /// The accumulation texture, so the compositor can read the in-progress stroke.
    #[must_use]
    pub fn accum_texture(&self) -> &wgpu::Texture {
        self.pool.texture()
    }

    /// Which accumulation layer holds the stroke for a tile, if any.
    #[must_use]
    pub fn accum_slot(&self, coord: TileCoord) -> Option<u32> {
        self.map.slot(coord).map(crate::tile_pool::Slot::layer)
    }

    /// Every tile the stroke in progress has accumulated into.
    ///
    /// The compositor needs these as well as the canvas's own tiles: a stroke reaches tiles the
    /// canvas has never had, and until it is baked there is nothing there to draw an instance
    /// for. Without them the preview is simply missing wherever the artist is drawing on fresh
    /// ground -- which is most of the time.
    pub fn accum_tiles(&self) -> impl Iterator<Item = TileCoord> + '_ {
        self.map.iter().map(|(c, _)| c)
    }

    /// The colour and opacity ceiling of the stroke in progress.
    #[must_use]
    pub fn paint(&self) -> ([f32; 4], f32) {
        self.paint
    }

    /// Whether the stroke in progress removes paint. The compositor needs it to preview
    /// correctly, since erasing is not "painting the paper colour".
    #[must_use]
    pub fn mode(&self) -> crate::editor::PaintMode {
        self.mode
    }

    /// Whether the layer holds any paint that still needs showing or baking.
    #[must_use]
    pub fn has_paint(&self) -> bool {
        self.dirty
    }

    /// Whether the accumulation pool ran out during the current stroke.
    #[must_use]
    pub fn exhausted(&self) -> bool {
        self.exhausted
    }

    /// Start a fresh stroke, releasing the previous one's accumulation tiles.
    ///
    /// Tiles are freed rather than cleared in place: a new stroke usually touches a
    /// different part of the canvas, and holding the old set resident would make the pool
    /// fill up with tiles nothing is going to accumulate into.
    pub fn begin_stroke(&mut self) {
        for (_, slot) in self.map.drain().collect::<Vec<_>>() {
            self.pool.free(slot);
        }
        self.stamp_records.clear();
        self.dirty = false;
        self.exhausted = false;
    }

    /// Discard the stroke in progress without baking it.
    pub fn abandon(&mut self) {
        self.begin_stroke();
    }

    /// Upload a whole frame's dabs in one go.
    ///
    /// Deliberately separate from stamping, and this separation is load-bearing:
    /// `Queue::write_buffer` does **not** interleave with recorded draws. Every write in a
    /// submission is applied *before* any command buffer in it executes, so writing the
    /// same buffer once per batch and drawing between writes means all the draws see only
    /// the last batch's data. That produced visible gaps in fast strokes, where several
    /// input batches land in one frame.
    ///
    /// Uploading once and drawing sub-ranges avoids the hazard entirely, and suits the
    /// data anyway: the editor already hands over a flat dab buffer with its commands
    /// indexing into it.
    pub fn upload_dabs(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, dabs: &[Dab]) {
        if dabs.is_empty() {
            return;
        }
        let data: Vec<DabInstance> = dabs.iter().map(DabInstance::from).collect();

        if data.len() > self.instance_capacity {
            // Grow generously; a long stroke segment can produce a lot of dabs at once and
            // reallocating per update would be wasteful.
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

    /// Write the camera and page rectangle used by the preview and by dab clipping.
    pub fn set_frame(&mut self, queue: &wgpu::Queue, xform: PageToNdc, page: PageRect) {
        self.frame = (xform, page);
        self.write_frame(queue);
    }

    /// Refresh just the page rectangle, keeping the camera as it was.
    ///
    /// Needed because dab clipping reads the page from this uniform, and stamping happens
    /// *before* the frame is drawn — so waiting for the next `set_frame` would clip the
    /// first frame of a stroke against a stale page. On the very first frame of all, that
    /// page would be a 1x1 placeholder and the whole stroke would vanish.
    pub fn set_page(&mut self, queue: &wgpu::Queue, page: PageRect) {
        self.frame.1 = page;
        self.write_frame(queue);
    }

    /// One writer for the whole uniform, so a partial update cannot leave the page and the
    /// camera describing different states.
    fn write_frame(&self, queue: &wgpu::Queue) {
        let (xform, page) = self.frame;
        let (ex, ey) = page.end();
        queue.write_buffer(
            &self.xform_buf,
            0,
            bytemuck::bytes_of(&XformUniform {
                x_row: [xform.x_row[0], xform.x_row[1], xform.x_row[2], 0.0],
                y_row: [xform.y_row[0], xform.y_row[1], xform.y_row[2], 0.0],
                page: [page.x as f32, page.y as f32, ex as f32, ey as f32],
                params: [
                    TILE_SIZE as f32,
                    f32::from(u8::from(self.stamped)),
                    0.0,
                    0.0,
                ],
                falloff: self.falloff,
            }),
        );
    }

    /// Set the tip for the stroke about to be drawn.
    ///
    /// Both arms are per *stroke*, not per dab: every dab of a stroke shares one tip, so an edge
    /// profile is sampled into a table once here rather than walked per fragment (a shader cannot
    /// evaluate a spline), and a stamp is uploaded once rather than per instance.
    ///
    /// Uploading the texture rebuilds the bind group, which is why the layout and sampler are held
    /// — a texture view cannot be swapped inside an existing group.
    pub fn set_tip(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        tip: &openpaint_core::dab::Tip,
    ) {
        match tip {
            openpaint_core::dab::Tip::Round(falloff) => {
                self.falloff = sample_falloff(falloff);
                self.stamped = false;
            }
            openpaint_core::dab::Tip::Stamp(stamp) => {
                let view = upload_stamp(device, queue, stamp);
                self.xform_group = make_xform_group(
                    device,
                    &self.xform_layout,
                    &self.xform_buf,
                    &view,
                    &self.stamp_sampler,
                );
                self.stamped = true;
            }
        }
        self.write_frame(queue);
    }

    /// Update the stroke colour and opacity used when compositing.
    pub fn set_paint(
        &mut self,
        queue: &wgpu::Queue,
        color_linear_premul: [f32; 4],
        opacity: f32,
        mode: crate::editor::PaintMode,
    ) {
        self.paint = (color_linear_premul, opacity.clamp(0.0, 1.0));
        self.mode = mode;
        queue.write_buffer(
            &self.paint_buf,
            0,
            bytemuck::bytes_of(&PaintUniform {
                color: color_linear_premul,
                opacity: [opacity.clamp(0.0, 1.0), 0.0, 0.0, 0.0],
            }),
        );
    }

    /// Make sure every tile `dabs` can reach has an accumulation layer, and publish the
    /// per-tile uniform records the stamping passes index into.
    ///
    /// Returns how many tiles to stamp. Called once per stroke segment per frame, before
    /// any pass in that segment, so the single uniform write cannot be reordered against
    /// the draws that read it.
    ///
    /// A fresh tile is cleared through the **encoder**, not a queue write: a queue write is
    /// applied before every command in the submission, which for a second stroke in the
    /// same frame would wipe a layer the first stroke had not finished with.
    pub fn prepare_tiles(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        dabs: &[Dab],
        page: PageRect,
    ) -> u32 {
        let touched = tiles_touched(dabs, page);
        self.prepare_tiles_at(queue, encoder, &touched)
    }

    /// Make sure each of `touched` has an accumulation layer, and publish the stamping records.
    ///
    /// Split out from [`StrokeLayer::prepare_tiles`] so a fill can use it: a fill knows its tiles
    /// from a selection mask rather than from dabs, but wants the identical allocate-clear-publish
    /// sequence, and duplicating that is how the two would drift.
    fn prepare_tiles_at(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        touched: &[TileCoord],
    ) -> u32 {
        let mut coords = Vec::with_capacity(touched.len());
        for &coord in touched {
            if !self.map.contains(coord) {
                let Some(slot) = self.pool.alloc() else {
                    self.exhausted = true;
                    continue;
                };
                // Fresh accumulation starts at zero coverage.
                self.pool
                    .clear_layer(encoder, &slot, wgpu::Color::TRANSPARENT);
                // Outside `debug_assert!`, which does not evaluate its expression in a
                // release build -- see DECISIONS 11a.6. This exact line, written the other
                // way, shipped a build that painted nothing at all.
                let displaced = self.map.insert(coord, slot);
                debug_assert!(displaced.is_none(), "accum tile {coord:?} already mapped");
                if let Some(slot) = displaced {
                    self.pool.free(slot);
                }
            }
            coords.push(coord);
        }
        self.stamp_records = self.publish(queue, &coords, Region::Stamp);
        self.stamp_records.len() as u32
    }

    /// Load a selection mask straight into the accumulation buffer, as a fill.
    ///
    /// **A fill is a stroke whose coverage came from a mask instead of from dabs.** Everything
    /// after this point is identical: the same accumulation buffer, the same bake, and therefore
    /// the same blend modes — so a fill honours alpha lock and erasing for free, and undo records it
    /// exactly as it records a stroke. No fill pipeline exists, and none should: a second path to
    /// the canvas is a second path that can disagree with the first.
    ///
    /// Coverage is written as it is, so a feathered or anti-aliased selection fills softly. That is
    /// the payoff for a mask being a byte per pixel rather than a bit.
    ///
    /// # Written through the encoder, not through the queue
    ///
    /// The obvious implementation is `Queue::write_texture`, and it silently does nothing.
    /// `prepare_tiles_at` records a *clear* of each freshly allocated tile into `encoder`, and a
    /// queue write is executed before any command buffer submitted after it — so the clear lands
    /// on top of the coverage and the fill vanishes. That is DECISIONS §11a.2 exactly, and it cost
    /// a debugging round here despite being written down.
    ///
    /// Staging through a buffer copied inside the same encoder puts both operations on one
    /// timeline, where the order is the order they were recorded in. Skipping the clear instead
    /// would also work today — a fill writes every texel — and would break the moment anything else
    /// touches these tiles in the same encoder.
    pub fn fill_from_mask(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        selection: &openpaint_core::Selection,
        page: PageRect,
    ) {
        let touched: Vec<TileCoord> = selection
            .tiles()
            .map(|(coord, _)| *coord)
            .filter(|c| crate::canvas_renderer::tile_intersects(*c, page))
            .collect();
        if touched.is_empty() {
            return;
        }
        self.prepare_tiles_at(queue, encoder, &touched);

        // One staging buffer for the whole fill: a tile is 128 KiB at this format, and a row is
        // 512 bytes, so tiles pack end to end with no padding and the 256-byte copy alignment is
        // satisfied by construction.
        let per_tile = TILE_SIZE * TILE_SIZE;
        let mut staged: Vec<half::f16> = Vec::with_capacity(touched.len() * per_tile);
        let mut layers: Vec<u32> = Vec::with_capacity(touched.len());
        for coord in &touched {
            let Some(layer) = self.map.slot(*coord).map(crate::tile_pool::Slot::layer) else {
                // No accumulation layer: the pool was exhausted, which `prepare_tiles_at` has
                // already recorded. Skipping the tile loses paint there rather than writing it
                // into somebody else's.
                continue;
            };
            let coverage = selection
                .coverage_tile(*coord)
                .expect("touched tiles exist");
            staged.extend(
                coverage
                    .iter()
                    .map(|&c| half::f16::from_f32(f32::from(c) / 255.0)),
            );
            layers.push(layer);
        }
        if layers.is_empty() {
            return;
        }

        let buffer = wgpu::util::DeviceExt::create_buffer_init(
            device,
            &wgpu::util::BufferInitDescriptor {
                label: Some("fill-coverage"),
                contents: bytemuck::cast_slice(&staged),
                usage: wgpu::BufferUsages::COPY_SRC,
            },
        );
        for (i, layer) in layers.iter().enumerate() {
            encoder.copy_buffer_to_texture(
                wgpu::ImageCopyBuffer {
                    buffer: &buffer,
                    layout: wgpu::ImageDataLayout {
                        offset: (i * per_tile * 2) as u64,
                        bytes_per_row: Some(TILE_SIZE as u32 * 2),
                        rows_per_image: Some(TILE_SIZE as u32),
                    },
                },
                wgpu::ImageCopyTexture {
                    texture: self.pool.texture(),
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: 0,
                        z: *layer,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d {
                    width: TILE_SIZE as u32,
                    height: TILE_SIZE as u32,
                    depth_or_array_layers: 1,
                },
            );
        }
        // Or `bake` decides there is nothing to do: the flag exists because stamping sets it, and
        // this is stamping by another route.
        self.dirty = true;
    }

    /// Write one uniform record per tile into `region`, returning the tiles in record
    /// order.
    ///
    /// A tile with no accumulation layer is skipped, so an exhausted pool loses paint in
    /// that one tile rather than misindexing every tile after it.
    fn publish(&self, queue: &wgpu::Queue, coords: &[TileCoord], region: Region) -> Vec<TileCoord> {
        let mut kept = Vec::with_capacity(coords.len());
        let mut bytes = Vec::with_capacity(coords.len() * TILE_PARAMS_STRIDE as usize);
        for coord in coords {
            let Some(slot) = self.map.slot(*coord) else {
                continue;
            };
            let t = TILE_SIZE as f32;
            let record = TileParamsRecord {
                tile: [coord.0 as f32 * t, coord.1 as f32 * t, t, 0.0],
                layer: [slot.layer(), 0, 0, 0],
            };
            bytes.extend_from_slice(bytemuck::bytes_of(&record));
            bytes.resize(
                bytes.len() + TILE_PARAMS_STRIDE as usize - size_of_record(),
                0,
            );
            kept.push(*coord);
        }
        if !bytes.is_empty() {
            queue.write_buffer(&self.tile_buf, self.region_base(region), &bytes);
        }
        kept
    }

    /// Byte offset where a region's records start.
    fn region_base(&self, region: Region) -> u64 {
        match region {
            Region::Stamp => 0,
            Region::Bake => u64::from(self.pool.capacity()) * TILE_PARAMS_STRIDE,
        }
    }

    /// Dynamic offset for record `index` of `region`.
    fn offset_of(&self, region: Region, index: u32) -> u32 {
        (self.region_base(region) + u64::from(index) * TILE_PARAMS_STRIDE) as u32
    }

    /// Stamp dabs `[start, start + len)` into the accumulation tile at record `index`.
    pub fn stamp_range(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        index: u32,
        start: usize,
        len: usize,
    ) {
        if len == 0 {
            return;
        }
        let Some(coord) = self.stamp_records.get(index as usize).copied() else {
            return;
        };
        let Some(slot) = self.map.slot(coord) else {
            return;
        };
        let mut pass = encoder
            .begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("dab-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: self.pool.layer_view(slot),
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // Load, not clear: accumulation is the whole point, and a tile can
                        // be stamped again in a later frame of the same stroke.
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
        pass.set_bind_group(0, &self.empty_group, &[]);
        pass.set_bind_group(1, &self.tile_group, &[self.offset_of(Region::Stamp, index)]);
        pass.set_bind_group(2, &self.xform_group, &[]);
        pass.set_vertex_buffer(0, self.instances.slice(..));
        pass.draw(0..6, start as u32..(start + len) as u32);
        drop(pass);

        self.dirty = true;
    }

    /// The canvas tiles a bake would write, in the order it will write them.
    ///
    /// Separate from [`StrokeLayer::bake`] because history has to snapshot those tiles
    /// *before* the bake overwrites them, and asking after the fact would be too late.
    #[must_use]
    pub fn tiles_to_bake(&self, page: PageRect) -> Vec<TileCoord> {
        if !self.dirty {
            return Vec::new();
        }
        let mut coords: Vec<TileCoord> = self
            .map
            .iter()
            .map(|(c, _)| c)
            .filter(|c| tile_intersects(*c, page))
            .collect();
        coords.sort_unstable_by_key(|c| (c.1, c.0));
        coords
    }

    /// Bake the accumulated stroke into the canvas, committing it.
    ///
    /// One pass per tile, because a render pass targets a single array layer. Stroke ends
    /// are rare compared with stroke updates, so the pass count is not on a hot path.
    ///
    /// Returns the canvas tiles that were written, which is what history records.
    pub fn bake(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        canvas: &mut CanvasRenderer,
        layer: LayerId,
    ) -> Vec<TileCoord> {
        if !self.dirty {
            return Vec::new();
        }
        let coords = self.tiles_to_bake(canvas.page());

        // Published into the bake region, so it cannot overwrite the stamping records that
        // earlier commands in this same submission are still going to read.
        let records = self.publish(queue, &coords, Region::Bake);

        let mut written = Vec::with_capacity(records.len());
        for (index, coord) in records.into_iter().enumerate() {
            // A tile the stroke reached may not exist on the canvas yet, and one that does
            // may have spilled to the CPU since it was last drawn.
            if canvas
                .ensure_tile(device, queue, encoder, layer, coord)
                .is_err()
            {
                self.exhausted = true;
                continue;
            }
            let Some(target) = canvas.tile_target(layer, coord) else {
                continue;
            };
            let mut pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("stroke-bake"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: target,
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
            pass.set_pipeline(match self.mode {
                crate::editor::PaintMode::Normal => &self.bake_pipeline,
                crate::editor::PaintMode::Erase => &self.erase_pipeline,
                crate::editor::PaintMode::LockAlpha => &self.lock_pipeline,
            });
            pass.set_bind_group(0, &self.paint_group, &[]);
            let offset = self.offset_of(Region::Bake, index as u32);
            pass.set_bind_group(1, &self.tile_group, &[offset]);
            pass.set_bind_group(2, &self.xform_group, &[]);
            pass.draw(0..6, 0..1);
            drop(pass);
            canvas.mark_dirty(layer, coord);
            written.push(coord);
        }

        self.dirty = false;
        written
    }
}

/// Which tiles inside the page a set of dabs can reach.
///
/// A dab's footprint is its radius plus a pixel of antialiasing slack, matching the quad
/// the vertex shader emits. Clipped to the page, because painting stops at the page edge
/// even though storage does not.
#[must_use]
pub fn tiles_touched(dabs: &[Dab], page: PageRect) -> Vec<TileCoord> {
    let mut set = HashSet::new();
    let (px1, py1) = page.end();
    for d in dabs {
        let r = d.radius + 1.0;
        let x0 = (d.x - r).floor().max(page.x as f32) as i32;
        let y0 = (d.y - r).floor().max(page.y as f32) as i32;
        let x1 = (d.x + r).ceil().min((px1 - 1) as f32) as i32;
        let y1 = (d.y + r).ceil().min((py1 - 1) as f32) as i32;
        if x1 < x0 || y1 < y0 {
            continue;
        }
        let (tx0, ty0) = tile_of(x0, y0);
        let (tx1, ty1) = tile_of(x1, y1);
        for ty in ty0..=ty1 {
            for tx in tx0..=tx1 {
                set.insert((tx, ty));
            }
        }
    }
    let mut out: Vec<TileCoord> = set.into_iter().collect();
    // Deterministic, so a frame's passes are ordered the same way every run.
    out.sort_unstable_by_key(|c| (c.1, c.0));
    out
}

/// Bytes one record occupies before padding to the stride.
const fn size_of_record() -> usize {
    std::mem::size_of::<TileParamsRecord>()
}

fn uniform_entry(
    binding: u32,
    visibility: wgpu::ShaderStages,
    has_dynamic_offset: bool,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset,
            min_binding_size: None,
        },
        count: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openpaint_core::{Canvas, StrokePainter};

    use crate::test_gpu::{
        any_paint, max_difference, mean_difference, readback_page, test_canvas, test_stroke_layer,
        try_device, L0, SIZE,
    };

    const BLACK: [f32; 4] = [0.0, 0.0, 0.0, 1.0];

    fn dab(x: f32, y: f32, radius: f32, hardness: f32, flow: f32) -> Dab {
        shaped(x, y, radius, hardness, flow, 1.0, 0.0)
    }

    /// A dab with an explicit shape, for the elliptical cases.
    fn shaped(
        x: f32,
        y: f32,
        radius: f32,
        hardness: f32,
        flow: f32,
        roundness: f32,
        angle: f32,
    ) -> Dab {
        Dab {
            x,
            y,
            radius,
            hardness,
            flow,
            roundness,
            angle,
            color_linear_premul: BLACK,
        }
    }

    /// Rasterize `dabs` on the GPU through the real tiled path and read the page back as
    /// linear f32 RGBA.
    fn gpu_render(dabs: &[Dab], opacity: f32) -> Vec<[f32; 4]> {
        gpu_render_batched(dabs, opacity, 1)
    }

    /// As `gpu_render`, with an explicit dab edge profile.
    fn gpu_render_with(
        dabs: &[Dab],
        opacity: f32,
        tip: &openpaint_core::dab::Tip,
    ) -> Vec<[f32; 4]> {
        gpu_render_inner(dabs, opacity, 1, tip)
    }

    /// As `gpu_render`, but stamping the dabs in `batches` separate draw calls within one
    /// frame -- which is what a fast stroke produces.
    fn gpu_render_batched(dabs: &[Dab], opacity: f32, batches: usize) -> Vec<[f32; 4]> {
        gpu_render_inner(dabs, opacity, batches, &openpaint_core::dab::Tip::default())
    }

    fn gpu_render_inner(
        dabs: &[Dab],
        opacity: f32,
        batches: usize,
        tip: &openpaint_core::dab::Tip,
    ) -> Vec<[f32; 4]> {
        let (device, queue) = try_device().expect("checked by caller");

        let page = openpaint_core::PageRect::from_size(SIZE, SIZE);
        let mut layer = test_stroke_layer(&device);
        let mut canvas = test_canvas(&device, page, &layer);

        // Every uniform written once, before the single submission -- the ordering rule
        // this module exists to respect.
        layer.set_page(&queue, page);
        layer.set_tip(&device, &queue, tip);
        layer.set_paint(&queue, BLACK, opacity, crate::editor::PaintMode::Normal);
        layer.upload_dabs(&device, &queue, dabs);

        let mut encoder = device.create_command_encoder(&Default::default());
        layer.begin_stroke();
        let tiles = layer.prepare_tiles(&queue, &mut encoder, dabs, page);

        // Split into `batches` draw calls, mimicking several input batches landing in a
        // single frame.
        let per = dabs.len().div_ceil(batches.max(1));
        for index in 0..tiles {
            let mut start = 0;
            while start < dabs.len() {
                let len = per.min(dabs.len() - start);
                layer.stamp_range(&mut encoder, index, start, len);
                start += len;
            }
        }
        layer.bake(&device, &queue, &mut encoder, &mut canvas, L0);
        queue.submit(std::iter::once(encoder.finish()));

        readback_page(&device, &queue, &canvas, L0)
    }

    /// Rasterize the same dabs through the CPU reference implementation.
    fn cpu_render(dabs: &[Dab], opacity: f32) -> Vec<[f32; 4]> {
        cpu_render_with(dabs, opacity, &openpaint_core::dab::Tip::default())
    }

    fn cpu_render_with(
        dabs: &[Dab],
        opacity: f32,
        tip: &openpaint_core::dab::Tip,
    ) -> Vec<[f32; 4]> {
        let mut canvas = Canvas::new(SIZE, SIZE);
        let mut painter = StrokePainter::new();
        painter.begin();
        painter.add_dabs(&canvas, dabs, tip);
        painter.composite(&mut canvas, BLACK, opacity);

        (0..SIZE)
            .flat_map(|y| (0..SIZE).map(move |x| (x, y)))
            .map(|(x, y)| match canvas.tile((0, 0)) {
                // Tiles are 256 wide, so the whole 128x128 canvas is tile (0, 0).
                Some(tile) => tile.texel(x as usize, y as usize),
                // A layer is transparent where nothing was painted.
                None => [0.0; 4],
            })
            .collect()
    }

    /// A tip with obvious structure: a plus sign, off-centre, at an odd size.
    ///
    /// Deliberately not square and not a power of two, because the row-padding `write_texture`
    /// demands is per row — a tip whose width happens to be a multiple of 256 would hide a skew
    /// that every real tip would show. Deliberately not symmetric either, so a flip or a transpose
    /// cannot pass.
    fn cross_stamp() -> openpaint_core::Stamp {
        const W: u32 = 13;
        const H: u32 = 9;
        let mut data = vec![0_u8; (W * H) as usize];
        for x in 0..W {
            data[(3 * W + x) as usize] = 255;
        }
        for y in 0..H {
            data[(y * W + 4) as usize] = 255;
        }
        // One soft corner, so the test covers interpolated values and not only 0 and 255.
        data[(W + 10) as usize] = 128;
        data[(W + 11) as usize] = 200;
        openpaint_core::Stamp::new(W, H, data).expect("valid stamp")
    }

    /// A bitmap tip, through both rasterizers.
    ///
    /// The pair that matters most of the three cross-checks, because the two sides are the least
    /// alike: the GPU reads the tip through a hardware sampler while the CPU walks the same
    /// arithmetic by hand. `Stamp::sample` is written to mirror a linear `ClampToEdge` sampler
    /// exactly — texel centres at `(i + 0.5) / n` — and this is the only thing holding it there.
    #[test]
    fn a_stamped_tip_matches_the_cpu_reference() {
        let Some(_) = try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };
        let tip = openpaint_core::dab::Tip::Stamp(std::sync::Arc::new(cross_stamp()));

        // Large enough that many page pixels fall inside one texel, which is where a sampling
        // convention that is off by half a texel shows up.
        let dabs = [dab(64.0, 64.0, 40.0, 1.0, 1.0)];
        let gpu = gpu_render_with(&dabs, 1.0, &tip);
        let cpu = cpu_render_with(&dabs, 1.0, &tip);

        assert!(any_paint(&gpu), "the stamp produced no paint at all");
        let (worst, at) = max_difference(&gpu, &cpu);
        assert!(
            worst < 0.01,
            "GPU and CPU disagree by {worst} at pixel {at} ({}, {}); gpu {:?} cpu {:?}",
            at as u32 % SIZE,
            at as u32 / SIZE,
            gpu[at],
            cpu[at]
        );
    }

    /// The stamp has to be *used*, not merely tolerated: a round dab and a cross-shaped one are
    /// very different pictures, and agreement between two renderers that both ignored the tip
    /// would prove nothing.
    #[test]
    fn a_stamped_tip_does_not_look_like_a_round_one() {
        let Some(_) = try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };
        let dabs = [dab(64.0, 64.0, 40.0, 1.0, 1.0)];
        let stamped = gpu_render_with(
            &dabs,
            1.0,
            &openpaint_core::dab::Tip::Stamp(std::sync::Arc::new(cross_stamp())),
        );
        let round = gpu_render(&dabs, 1.0);
        let (difference, _) = max_difference(&stamped, &round);
        assert!(
            difference > 0.5,
            "the stamped dab rendered almost identically to a round one, so the tip was ignored"
        );

        // And specifically: the corners of the dab's square are empty in the cross but covered by
        // a hard round dab's... no — a round dab does *not* cover its square's corners. Check
        // instead where the two genuinely differ: off the cross's arms but well inside the disc.
        let off_arm = (64 + 14) as usize + (64 + 14) as usize * SIZE as usize;
        assert!(
            round[off_arm][3] > 0.9,
            "a hard round dab should cover a point 14px from its centre"
        );
        assert!(
            stamped[off_arm][3] < 0.1,
            "and the cross should not, since nothing is drawn there in the tip"
        );
    }

    /// Roundness and angle are dab geometry, not tip geometry, so a stamp has to turn with them.
    /// If it did not, the two kinds of tip would answer differently to the same controls.
    #[test]
    fn a_stamped_tip_turns_with_the_dab() {
        let Some(_) = try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };
        let stamp = std::sync::Arc::new(cross_stamp());
        let tip = openpaint_core::dab::Tip::Stamp(stamp);

        let upright = [dab(64.0, 64.0, 40.0, 1.0, 1.0)];
        let turned = [Dab {
            angle: std::f32::consts::FRAC_PI_2,
            ..upright[0]
        }];

        let a = gpu_render_with(&upright, 1.0, &tip);
        let b = gpu_render_with(&turned, 1.0, &tip);
        let (difference, _) = max_difference(&a, &b);
        assert!(
            difference > 0.5,
            "rotating the dab did not rotate the stamp"
        );

        // And the CPU agrees about the turned one too, which is what pins the shared frame
        // transform rather than just the sampling.
        let cpu = cpu_render_with(&turned, 1.0, &tip);
        let (worst, at) = max_difference(&b, &cpu);
        assert!(
            worst < 0.01,
            "a rotated stamp disagrees by {worst} at pixel {at}"
        );
    }

    /// A shaped edge profile, through both rasterizers.
    ///
    /// The shader cannot walk a spline, so it reads a thirty-two entry table with linear
    /// interpolation while the CPU evaluates the curve exactly. They are therefore *not* the same
    /// arithmetic -- which is precisely why this needs pinning: the table has to be fine enough
    /// that the difference stays under what the accumulation buffer can record.
    ///
    /// Deliberately a curve with real shape in it. A gentle one would pass whatever the table
    /// resolution, and prove nothing about it.
    #[test]
    fn gpu_shaped_falloff_matches_the_cpu_reference() {
        let Some(_) = try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };

        let profile = openpaint_core::Curve::from_points(vec![
            (0.0, 1.0),
            (0.15, 0.95),
            (0.5, 0.35),
            (0.8, 0.08),
            (1.0, 0.0),
        ])
        .expect("valid profile");

        // Soft dabs, so most of each one *is* the ramp being tested.
        let dabs = [
            dab(48.0, 64.0, 22.0, 0.0, 1.0),
            dab(80.0, 64.0, 22.0, 0.2, 1.0),
        ];

        let gpu = gpu_render_with(
            &dabs,
            1.0,
            &openpaint_core::dab::Tip::Round(profile.clone()),
        );
        let cpu = cpu_render_with(
            &dabs,
            1.0,
            &openpaint_core::dab::Tip::Round(profile.clone()),
        );

        assert!(any_paint(&gpu), "GPU produced no paint at all");
        let (worst, at) = max_difference(&gpu, &cpu);
        assert!(
            worst < 0.01,
            "GPU and CPU disagree by {worst} at pixel {at} ({}, {}); gpu {:?} cpu {:?}",
            at as u32 % SIZE,
            at as u32 / SIZE,
            gpu[at],
            cpu[at]
        );

        // And the profile has to actually change the picture, or the agreement above is only
        // two renderers ignoring it together.
        let plain = gpu_render(&dabs, 1.0);
        let (shaped_vs_plain, _) = max_difference(&gpu, &plain);
        assert!(
            shaped_vs_plain > 0.1,
            "the shaped profile rendered the same as the straight ramp, so neither used it"
        );
    }

    /// The elliptical dab, through both rasterizers.
    ///
    /// The shape transform exists twice — `Dab::distance_to` and `dab_fs` in stroke.wgsl — and two
    /// copies of a formula drift. A rotated, flattened dab is the case where a sign error in the
    /// rotation or a swapped axis is unmistakable, and invisible on a circle.
    #[test]
    fn gpu_elliptical_dabs_match_the_cpu_reference() {
        let Some(_) = try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };

        // Deliberately awkward angles: a quarter turn would hide a swapped axis, and a whole turn
        // would hide a wrong sign.
        let dabs = [
            shaped(40.0, 64.0, 18.0, 1.0, 1.0, 0.3, 0.0),
            shaped(60.0, 64.0, 18.0, 0.5, 1.0, 0.3, 0.7),
            shaped(80.0, 64.0, 18.0, 0.0, 1.0, 0.15, 2.2),
            shaped(96.0, 70.0, 12.0, 0.5, 1.0, 0.6, -1.1),
        ];

        let gpu = gpu_render(&dabs, 1.0);
        let cpu = cpu_render(&dabs, 1.0);

        assert!(any_paint(&gpu), "GPU produced no paint at all");
        assert!(any_paint(&cpu), "CPU reference produced no paint");

        let (worst, at) = max_difference(&gpu, &cpu);
        assert!(
            worst < 0.01,
            "GPU and CPU disagree by {worst} at pixel {at} ({}, {}); gpu {:?} cpu {:?}",
            at as u32 % SIZE,
            at as u32 / SIZE,
            gpu[at],
            cpu[at]
        );
    }

    /// The reason `openpaint_core::raster` and `openpaint_core::stroke` are kept: the
    /// falloff curve exists twice, once in `Dab::coverage_at_distance` and once in
    /// `stroke.wgsl`'s `dab_fs`. Two copies of a curve drift. This is what notices.
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
            "GPU and CPU disagree by {worst} at pixel {at} ({}, {}); gpu {:?} cpu {:?}",
            at as u32 % SIZE,
            at as u32 / SIZE,
            gpu[at],
            cpu[at]
        );
    }

    /// Build-up with a low flow and an opacity ceiling.
    ///
    /// Uses *soft* dabs deliberately. With `hardness = 1.0` coverage is a step function, so
    /// a sub-pixel sampling difference between the two implementations flips a whole dab on
    /// or off at the rim -- and with several overlapping dabs that is a ~12% swing in the
    /// accumulated value. That is inherent to comparing hard edges, not a defect, so the
    /// strict comparison uses a continuous falloff where a small position error can only
    /// produce a small value error.
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

        // Max difference can be dominated by a handful of rim pixels, so also require the
        // two to agree closely on average.
        let mean = mean_difference(&gpu, &cpu);
        assert!(mean < 0.002, "mean difference {mean} is too large");

        // And the ceiling must actually bind: a stroke at opacity 0.5 can never reach more
        // than half coverage, however many dabs pile up.
        let most = gpu.iter().map(|p| p[3]).fold(0.0f32, f32::max);
        assert!(
            most <= opacity + 0.02,
            "GPU passed the opacity ceiling: most {most}, ceiling {opacity}"
        );
    }

    /// Regression: a fast stroke delivers several input batches in one frame, so the dabs
    /// are stamped in several draw calls. Splitting them must be indistinguishable from one
    /// call.
    ///
    /// This failed before `upload_dabs` was separated from stamping: writing the instance
    /// buffer once per batch looks correct, but `Queue::write_buffer` applies every write in
    /// a submission *before* any of its command buffers run, so all the draws saw only the
    /// final batch. It showed up as visible gaps in fast strokes -- worse the faster the
    /// stroke, because more batches landed per frame.
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
                "{batches} batches differ from 1 by {worst} at ({}, {}); one {:?} split {:?}",
                at as u32 % SIZE,
                at as u32 / SIZE,
                one_call[at],
                split[at]
            );
        }
    }

    /// A stroke spanning several tiles must look exactly like the CPU reference across the
    /// seams. This is the property the whole tiled rewrite risks: each tile is stamped in
    /// its own pass with its own origin, so an off-by-one in that origin, or clipping a dab
    /// quad at a tile edge instead of letting the target clip it, shows up as a visible line
    /// every 256 pixels.
    #[test]
    fn a_stroke_across_tile_boundaries_has_no_seam() {
        let Some((device, queue)) = try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };

        // A page four tiles across, and a stroke straight through all of them.
        const W: u32 = 900;
        const H: u32 = 300;
        let dabs: Vec<Dab> = (0..300)
            .map(|i| dab(10.0 + i as f32 * 3.0, 150.0, 20.0, 0.5, 0.4))
            .collect();

        let page = openpaint_core::PageRect::from_size(W, H);
        let mut layer = test_stroke_layer(&device);
        let mut canvas = test_canvas(&device, page, &layer);

        layer.set_page(&queue, page);
        layer.set_paint(&queue, BLACK, 1.0, crate::editor::PaintMode::Normal);
        layer.upload_dabs(&device, &queue, &dabs);
        let mut encoder = device.create_command_encoder(&Default::default());
        layer.begin_stroke();
        let tiles = layer.prepare_tiles(&queue, &mut encoder, &dabs, page);
        assert!(tiles >= 4, "expected several tiles, got {tiles}");
        for index in 0..tiles {
            layer.stamp_range(&mut encoder, index, 0, dabs.len());
        }
        layer.bake(&device, &queue, &mut encoder, &mut canvas, L0);
        queue.submit(std::iter::once(encoder.finish()));

        let gpu = readback_page(&device, &queue, &canvas, L0);

        let mut reference = Canvas::new(W, H);
        let mut painter = StrokePainter::new();
        painter.begin();
        painter.add_dabs(&reference, &dabs, &openpaint_core::dab::Tip::default());
        painter.composite(&mut reference, BLACK, 1.0);

        let cpu: Vec<[f32; 4]> = (0..H)
            .flat_map(|y| (0..W).map(move |x| (x, y)))
            .map(|(x, y)| {
                let coord = (
                    (x as i32).div_euclid(TILE_SIZE as i32),
                    (y as i32).div_euclid(TILE_SIZE as i32),
                );
                match reference.tile(coord) {
                    Some(tile) => tile.texel((x as usize) % TILE_SIZE, (y as usize) % TILE_SIZE),
                    None => [0.0; 4],
                }
            })
            .collect();

        assert!(any_paint(&gpu), "GPU produced no paint at all");
        let (worst, at) = max_difference(&gpu, &cpu);
        assert!(
            worst < 0.02,
            "seam at pixel ({}, {}): gpu {:?} cpu {:?}",
            at as u32 % W,
            at as u32 / W,
            gpu[at],
            cpu[at]
        );
    }

    /// Painting is clipped to the page even though storage is not: a dab centred outside
    /// the page must deposit nothing inside it, and must not allocate a tile either.
    #[test]
    fn dabs_outside_the_page_paint_nothing() {
        let Some(_) = try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };
        let far = [dab(-500.0, -500.0, 20.0, 1.0, 1.0)];
        let pixels = gpu_render(&far, 1.0);
        assert!(!any_paint(&pixels), "paint landed from a dab off the page");
    }

    /// `tiles_touched` decides how many passes a frame records, so a dab must claim exactly
    /// the tiles its footprint reaches -- one too few loses paint at a seam, and one too
    /// many burns a render pass and an accumulation layer on nothing.
    #[test]
    fn a_dab_claims_exactly_the_tiles_it_reaches() {
        let page = PageRect::from_size(1000, 1000);

        // Comfortably inside one tile.
        assert_eq!(
            tiles_touched(&[dab(100.0, 100.0, 10.0, 1.0, 1.0)], page),
            vec![(0, 0)]
        );

        // Straddling the boundary at x = 256 reaches both columns.
        assert_eq!(
            tiles_touched(&[dab(256.0, 100.0, 10.0, 1.0, 1.0)], page),
            vec![(0, 0), (1, 0)]
        );

        // A dab centred on a corner reaches all four.
        assert_eq!(
            tiles_touched(&[dab(256.0, 256.0, 10.0, 1.0, 1.0)], page),
            vec![(0, 0), (1, 0), (0, 1), (1, 1)]
        );

        // Entirely off the page: nothing at all.
        assert!(tiles_touched(&[dab(-500.0, -500.0, 20.0, 1.0, 1.0)], page).is_empty());
    }

    /// A page with a negative origin is the normal case after extending up or left, and its
    /// tiles are negative too. `div_euclid` is what makes that work.
    #[test]
    fn tiles_touched_handles_a_negative_origin() {
        let page = PageRect::new(-400, -400, 800, 800);
        assert_eq!(
            tiles_touched(&[dab(-300.0, -300.0, 10.0, 1.0, 1.0)], page),
            vec![(-2, -2)]
        );
        // Clipped at the page's own edge, not at zero.
        assert!(tiles_touched(&[dab(-450.0, -450.0, 5.0, 1.0, 1.0)], page).is_empty());
    }
}
