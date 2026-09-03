//! Resampling the floating selection on the GPU.
//!
//! A live transform used to rebuild its preview on the CPU: every destination pixel, a twenty-tap
//! filter over the source, once per pointer sample. Measured at 33 ms for a 256-pixel selection
//! and 453 ms for a 1024-pixel one — usable small, not a live drag at any real size. That is not a
//! tuning problem. Resampling is O(area) with a wide filter, so no amount of care makes the CPU
//! the right machine for a preview that has to keep up with a pen (`TODO.md` §3, DECISIONS §4a).
//!
//! # What this is
//!
//! The lifted pixels go up as one texture at the lift. Each frame, every destination tile the
//! transform touches gets a render pass that draws one quad, and the fragment shader asks
//! `Transform::invert` where each pixel came from. The float layer's tiles are ordinary tiles of
//! an ordinary layer, exactly as before — the compositor is untouched, because a floating
//! selection *is* a layer (§5g).
//!
//! # What it deliberately does not do
//!
//! **The commit still resamples on the CPU, with Mitchell.** It runs once per gesture, and that is
//! where the quality is worth paying for (§5d). This is a *preview*, filtered bilinear, so it is
//! very slightly softer than the result it is previewing — which is what every comparable
//! application does, and the open question `TODO.md` §3 named.
//!
//! **It falls back rather than failing.** A selection larger than the device will hold as one
//! texture, or a machine that refuses the pipeline, goes back to the CPU path: slow is a
//! complaint, and a preview that does not appear is a bug.

use openpaint_core::tile::{TileCoord, TILE_SIZE};
use openpaint_core::{Lifted, Transform};

/// One transparent pixel of margin on every side of the uploaded source.
///
/// **This is why the shader needs no bounds check.** The sampler clamps to the edge, so without a
/// margin every sample past the artwork would read the outermost row of pixels and smear it
/// across the page — most visibly at the corners of a rotation, where the destination tile is
/// mostly outside the source. With it, "outside" reads as transparent, which is what it is.
const MARGIN: u32 = 1;

/// Bytes in one texel of the canvas format, `Rgba16Float`.
const BYTES_PER_TEXEL: u32 = 8;

/// What one destination tile needs to know. Must match `Params` in `float.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    /// Tile origin in page pixels, then the tile's size, then padding.
    dest: [f32; 4],
    /// Source origin in page pixels, then its size in pixels.
    src: [f32; 4],
    pivot_offset: [f32; 4],
    scale_rot: [f32; 4],
}

/// The lifted pixels on the GPU, and the pipeline that draws them.
pub struct FloatPass {
    pipeline: wgpu::RenderPipeline,
    params_layout: wgpu::BindGroupLayout,
    source_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    /// One aligned slot per destination tile, so every pass in a frame reads its own.
    ///
    /// **Not one slot rewritten between passes.** `Queue::write_buffer` is ordered against
    /// submissions, not against the commands already recorded into an encoder, so writing the
    /// same slot before each pass would give every pass the *last* value — recurring hazard
    /// §11a.2, and it would look like every tile drawing the same corner of the picture.
    params: wgpu::Buffer,
    params_group: wgpu::BindGroup,
    slots: usize,
    /// Distance between slots, from the device's uniform alignment.
    stride: u32,
    /// The lifted pixels, with their margin, and where their top-left sits in page pixels.
    source: Option<Source>,
}

struct Source {
    group: wgpu::BindGroup,
    /// Page coordinates of the texture's top-left, margin included.
    origin: (f32, f32),
    /// Texture size in pixels, margin included.
    size: (f32, f32),
    /// The content bounds this was built from, so a different lift is noticed.
    bounds: (i32, i32, i32, i32),
}

impl FloatPass {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("float-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("float.wgsl").into()),
        });
        let params_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("float-params-layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: wgpu::BufferSize::new(std::mem::size_of::<Params>() as u64),
                },
                count: None,
            }],
        });
        let source_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("float-source-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("float-pipeline-layout"),
            bind_group_layouts: &[&params_layout, &source_layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("float-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "float_vs",
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "float_fs",
                // **Replace, not blend.** Every destination tile is cleared and written in one
                // pass: the float layer holds only the transformed pixels, and blending them over
                // whatever the tile held last frame is how a drag leaves a smear behind it.
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("float-sampler"),
            // Bilinear, and clamped onto the transparent margin. See `MARGIN`.
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        let stride = device
            .limits()
            .min_uniform_buffer_offset_alignment
            .max(std::mem::size_of::<Params>() as u32);
        let (params, params_group, slots) = Self::make_params(device, &params_layout, stride, 64);
        Self {
            pipeline,
            params_layout,
            source_layout,
            sampler,
            params,
            params_group,
            slots,
            stride,
            source: None,
        }
    }

    fn make_params(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        stride: u32,
        slots: usize,
    ) -> (wgpu::Buffer, wgpu::BindGroup, usize) {
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("float-params"),
            size: u64::from(stride) * slots as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("float-params-group"),
            layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &buffer,
                    offset: 0,
                    size: wgpu::BufferSize::new(std::mem::size_of::<Params>() as u64),
                }),
            }],
        });
        (buffer, group, slots)
    }

    /// Put the lifted pixels on the GPU. Returns whether the GPU path is available for them.
    ///
    /// Called once per lift rather than per frame: the pixels do not change while they are being
    /// dragged, which is the entire reason this is worth doing.
    pub fn hold(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, lifted: &Lifted) -> bool {
        self.source = None;
        let Some(bounds) = lifted.content_bounds() else {
            return false;
        };
        let (x0, y0, x1, y1) = bounds;
        #[expect(
            clippy::cast_sign_loss,
            reason = "content_bounds is ordered, so the differences are positive"
        )]
        let (w, h) = ((x1 - x0) as u32 + MARGIN * 2, (y1 - y0) as u32 + MARGIN * 2);
        let limit = device.limits().max_texture_dimension_2d;
        if w > limit || h > limit {
            // Bigger than one texture on this device. The CPU path is slow at this size, and slow
            // is a complaint where a missing preview is a bug.
            return false;
        }

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("float-source"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        // Rows padded to the 256-byte alignment `write_texture` requires. Built once, at the
        // lift, so the cost is a copy of the selection rather than a copy per frame.
        let row = (w * BYTES_PER_TEXEL).next_multiple_of(256);
        let mut bytes = vec![0_u8; row as usize * h as usize];
        for y in 0..h {
            for x in 0..w {
                #[expect(
                    clippy::cast_possible_wrap,
                    reason = "bounded by the device's texture limit"
                )]
                let page = (x0 + x as i32 - MARGIN as i32, y0 + y as i32 - MARGIN as i32);
                let texel = lifted.texel_at(page.0, page.1);
                let at = (y * row + x * BYTES_PER_TEXEL) as usize;
                for (i, v) in texel.iter().enumerate() {
                    let half = half::f16::from_f32(*v).to_le_bytes();
                    bytes[at + i * 2] = half[0];
                    bytes[at + i * 2 + 1] = half[1];
                }
            }
        }
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &bytes,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("float-source-group"),
            layout: &self.source_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        #[expect(
            clippy::cast_precision_loss,
            reason = "page coordinates, bounded well inside f32's exact integers"
        )]
        let origin = ((x0 - MARGIN as i32) as f32, (y0 - MARGIN as i32) as f32);
        #[expect(clippy::cast_precision_loss, reason = "a texture side, at most 16384")]
        let size = (w as f32, h as f32);
        self.source = Some(Source {
            group,
            origin,
            size,
            bounds,
        });
        true
    }

    /// Whether the pixels held are the ones being asked about.
    ///
    /// A cheap identity rather than a deep comparison: `float_at` is only ever called with the
    /// lifted set that was just held, and this catches the case where it is not — a stale hold
    /// after an undo, say — by falling back rather than by drawing the wrong picture.
    pub fn holds(&self, lifted: &Lifted) -> bool {
        self.source
            .as_ref()
            .is_some_and(|s| Some(s.bounds) == lifted.content_bounds())
    }

    /// Which destination tiles a transform of the held pixels touches.
    #[must_use]
    pub fn tiles_for(&self, transform: &Transform) -> Vec<TileCoord> {
        let Some(source) = self.source.as_ref() else {
            return Vec::new();
        };
        let (x0, y0, x1, y1) = source.bounds;
        let (dx0, dy0, dx1, dy1) = transform.bounds_of(x0, y0, x1, y1);
        let size = TILE_SIZE as i32;
        let mut out = Vec::new();
        // One extra tile of reach on each side, because a bilinear sample at the very edge of the
        // transformed rectangle still reads a texel just outside it.
        for ty in (dy0 - 1).div_euclid(size)..=(dy1).div_euclid(size) {
            for tx in (dx0 - 1).div_euclid(size)..=(dx1).div_euclid(size) {
                out.push((tx, ty));
            }
        }
        out
    }

    /// Record the passes that draw one destination tile each.
    ///
    /// `target` supplies the render target for a tile, or `None` if there is no room for it —
    /// which is the tile store being full, and is the same answer the CPU path would give by
    /// having nowhere to put the tile.
    pub fn draw<'a>(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        transform: &Transform,
        coords: &[TileCoord],
        mut target: impl FnMut(TileCoord) -> Option<&'a wgpu::TextureView>,
    ) -> bool {
        let Some(source) = self.source.as_ref() else {
            return false;
        };
        if coords.len() > self.slots {
            let (buffer, group, slots) = Self::make_params(
                device,
                &self.params_layout,
                self.stride,
                coords.len().max(1),
            );
            self.params = buffer;
            self.params_group = group;
            self.slots = slots;
        }

        let (sx, sy) = transform.effective_scale();
        let (sinr, cosr) = transform.rotation.sin_cos();
        #[expect(
            clippy::cast_precision_loss,
            reason = "tile coordinates, far inside f32's exact integers"
        )]
        let params: Vec<Params> = coords
            .iter()
            .map(|(tx, ty)| Params {
                dest: [
                    (tx * TILE_SIZE as i32) as f32,
                    (ty * TILE_SIZE as i32) as f32,
                    TILE_SIZE as f32,
                    0.0,
                ],
                src: [
                    source.origin.0,
                    source.origin.1,
                    source.size.0,
                    source.size.1,
                ],
                pivot_offset: [
                    transform.pivot.0,
                    transform.pivot.1,
                    transform.offset.0,
                    transform.offset.1,
                ],
                scale_rot: [sx, sy, sinr, cosr],
            })
            .collect();
        // **Every slot written before any pass runs.** See the note on `params`.
        for (i, p) in params.iter().enumerate() {
            queue.write_buffer(
                &self.params,
                u64::from(self.stride) * i as u64,
                bytemuck::bytes_of(p),
            );
        }

        for (i, coord) in coords.iter().enumerate() {
            let Some(view) = target(*coord) else { continue };
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("float-tile"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // Cleared, not loaded: this tile is being replaced, and what it held is
                        // last frame's position of the same pixels.
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.pipeline);
            #[expect(
                clippy::cast_possible_truncation,
                reason = "one offset per tile of a page, far inside u32"
            )]
            pass.set_bind_group(0, &self.params_group, &[self.stride * i as u32]);
            pass.set_bind_group(1, &source.group, &[]);
            pass.draw(0..6, 0..1);
        }
        true
    }

    /// Forget the held pixels, at the end of a gesture.
    pub fn let_go(&mut self) {
        self.source = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas_renderer::CANVAS_FORMAT;
    use crate::test_gpu;
    use crate::tile_store::LayerId;
    use openpaint_core::tile::Tile;

    const FLOAT: LayerId = LayerId(9);

    /// The colour every test square is painted in.
    ///
    /// Premultiplied, opaque, and deliberately not grey: two channels swapped on the way through
    /// the shader would be invisible in grey and obvious in this.
    const PAINT: [f32; 4] = [0.8, 0.2, 0.05, 1.0];

    /// A lifted square of solid colour, `side` across, with its top-left at `at`.
    ///
    /// Built through `Lifted::from_layer` and a rectangular selection, which is how the
    /// application makes one -- a test that assembled the tiles itself could hold a shape the
    /// real path cannot produce.
    fn a_square(at: (i32, i32), side: i32) -> Lifted {
        #[expect(clippy::cast_sign_loss, reason = "the test's own positive coordinates")]
        let rect = openpaint_core::PageRect::new(at.0, at.1, side as u32, side as u32);
        let page = openpaint_core::PageRect::new(0, 0, 512, 512);
        let selection = openpaint_core::Selection::from_rect(rect, page);
        Lifted::from_layer(&selection, |_| {
            let mut tile = Tile::transparent();
            for y in 0..TILE_SIZE {
                for x in 0..TILE_SIZE {
                    tile.set_texel(x, y, PAINT);
                }
            }
            Some(tile)
        })
    }

    /// The GPU preview draws the same picture the CPU resample did.
    ///
    /// **This is the whole safety of the change.** The preview moved machines; what it shows must
    /// not move with it. Compared against `Lifted::transformed`, which is the code this replaced
    /// and which the *commit* still uses — so if the two ever part company, a transform would
    /// jump at the moment it was applied.
    ///
    /// The tolerance is real and stated: the GPU filters bilinear and the CPU filters Mitchell
    /// (`TODO.md` §3 chose that deliberately), so the two differ along edges by up to the
    /// difference between those filters. What is being asserted is that the picture is in the
    /// same place, the same size, the same way round and the same colour — not that two different
    /// filters agree pixel for pixel, which would be asserting they were the same filter.
    #[test]
    fn the_gpu_preview_shows_what_the_cpu_resample_showed() {
        let Some((device, queue)) = test_gpu::try_device() else {
            eprintln!("no GPU adapter; skipping");
            return;
        };
        let stroke = test_gpu::test_stroke_layer(device);
        let page = openpaint_core::PageRect::new(0, 0, 512, 512);
        let mut canvas = test_gpu::test_canvas(device, page, &stroke);
        let mut pass = FloatPass::new(device, CANVAS_FORMAT);

        let lifted = a_square((100, 100), 120);
        assert!(
            pass.hold(device, queue, &lifted),
            "the source would not go up"
        );
        assert!(pass.holds(&lifted));

        // A rotation and a scale together: a translation alone would pass with the inverse
        // transposed, the sines negated, or the pivot ignored.
        let transform = openpaint_core::Transform {
            pivot: (160.0, 160.0),
            offset: (30.0, -20.0),
            scale: (1.4, 0.8),
            rotation: 0.6,
        };

        let coords = pass.tiles_for(&transform);
        assert!(!coords.is_empty(), "the transform touched no tiles");
        let mut encoder = device.create_command_encoder(&Default::default());
        for coord in &coords {
            assert!(
                canvas
                    .ensure_tile(device, queue, &mut encoder, FLOAT, *coord)
                    .is_ok(),
                "no room for a tile in a test with a generous budget"
            );
        }
        {
            let view = &canvas;
            assert!(
                pass.draw(device, queue, &mut encoder, &transform, &coords, |c| view
                    .tile_target(FLOAT, c))
            );
        }
        queue.submit(std::iter::once(encoder.finish()));

        let wanted = lifted.transformed(&transform, openpaint_core::Kernel::Mitchell);
        let mut compared = 0;
        for coord in &coords {
            let Some(got) = test_gpu::readback_tile(device, queue, &canvas, FLOAT, *coord) else {
                continue;
            };
            let cpu = wanted.get(coord).cloned().unwrap_or_else(Tile::transparent);
            let want: Vec<[f32; 4]> = (0..TILE_SIZE * TILE_SIZE)
                .map(|i| cpu.texel(i % TILE_SIZE, i / TILE_SIZE))
                .collect();
            // The mean, not the maximum: the maximum is the width of the filter difference at an
            // edge, and asserting on it would be asserting that bilinear is Mitchell.
            let mean = test_gpu::mean_difference(&got, &want);
            assert!(
                mean < 0.02,
                "tile {coord:?} differs from the CPU resample by {mean:.4} on average"
            );
            compared += 1;
        }
        assert!(compared > 0, "no tile was compared, so nothing was proved");
    }

    /// The pixels land where the transform says, and nowhere else.
    ///
    /// A separate assertion from the comparison above because a *shared* mistake — both sides
    /// wrong the same way — would pass it. This one knows independently where a scaled, moved
    /// square should be: at its four corners, and outside it, transparent.
    #[test]
    fn the_preview_lands_where_the_transform_puts_it() {
        let Some((device, queue)) = test_gpu::try_device() else {
            eprintln!("no GPU adapter; skipping");
            return;
        };
        let stroke = test_gpu::test_stroke_layer(device);
        let page = openpaint_core::PageRect::new(0, 0, 512, 512);
        let mut canvas = test_gpu::test_canvas(device, page, &stroke);
        let mut pass = FloatPass::new(device, CANVAS_FORMAT);

        let lifted = a_square((64, 64), 64);
        assert!(pass.hold(device, queue, &lifted));
        // Twice the size about the square's own centre, and moved 40 to the right: the square
        // was 64..128, so it becomes 32..160 and then 72..200.
        let transform = openpaint_core::Transform {
            pivot: (96.0, 96.0),
            offset: (40.0, 0.0),
            scale: (2.0, 2.0),
            rotation: 0.0,
        };
        let coords = pass.tiles_for(&transform);
        let mut encoder = device.create_command_encoder(&Default::default());
        for coord in &coords {
            let _ = canvas.ensure_tile(device, queue, &mut encoder, FLOAT, *coord);
        }
        {
            let view = &canvas;
            pass.draw(device, queue, &mut encoder, &transform, &coords, |c| {
                view.tile_target(FLOAT, c)
            });
        }
        queue.submit(std::iter::once(encoder.finish()));

        let read = |x: i32, y: i32| {
            let coord = (
                x.div_euclid(TILE_SIZE as i32),
                y.div_euclid(TILE_SIZE as i32),
            );
            let tile = test_gpu::readback_tile(device, queue, &canvas, FLOAT, coord)?;
            let side = TILE_SIZE as i32;
            let at = (y.rem_euclid(side) * side + x.rem_euclid(side)) as usize;
            Some(tile[at])
        };

        // Well inside: opaque, and the colour that went in.
        let inside = read(120, 100).expect("a tile in the middle of it");
        assert!(inside[3] > 0.9, "the middle of the square is not opaque");
        assert!(
            (inside[0] - PAINT[0]).abs() < 0.02 && (inside[1] - PAINT[1]).abs() < 0.02,
            "the colour changed on the way through: {inside:?}"
        );

        // Well outside, on all four sides.
        for (x, y, side) in [
            (60, 100, "left"),
            (210, 100, "right"),
            (120, 20, "above"),
            (120, 175, "below"),
        ] {
            let out = read(x, y).unwrap_or([0.0; 4]);
            assert!(
                out[3] < 0.02,
                "paint appeared {side} the square at ({x},{y}): {out:?}"
            );
        }
    }

    /// Without a hold there is nothing to draw, and the caller is told rather than shown nothing.
    ///
    /// The fallback to the CPU depends on this answer: a `draw` that quietly did nothing would
    /// leave the float layer empty, which on screen is a selection that vanishes when you grab it.
    #[test]
    fn a_pass_with_nothing_held_refuses() {
        let Some((device, queue)) = test_gpu::try_device() else {
            eprintln!("no GPU adapter; skipping");
            return;
        };
        let mut pass = FloatPass::new(device, CANVAS_FORMAT);
        assert!(!pass.holds(&a_square((0, 0), 8)));
        assert!(pass
            .tiles_for(&openpaint_core::Transform::IDENTITY)
            .is_empty());
        let mut encoder = device.create_command_encoder(&Default::default());
        assert!(!pass.draw(
            device,
            queue,
            &mut encoder,
            &openpaint_core::Transform::IDENTITY,
            &[(0, 0)],
            |_| None
        ));

        // A lift that found nothing is nothing to hold either.
        let page = openpaint_core::PageRect::new(0, 0, 512, 512);
        let selection =
            openpaint_core::Selection::from_rect(openpaint_core::PageRect::new(10, 10, 4, 4), page);
        let empty = Lifted::from_layer(&selection, |_| None);
        assert!(!pass.hold(device, queue, &empty));
    }
}
