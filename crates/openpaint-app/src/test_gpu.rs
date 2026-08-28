//! Shared plumbing for the GPU tests.
//!
//! Exists so `stroke_layer` and `history` can both test against a real device
//! without duplicating device setup and texture readback. Test-only.
//!
//! Tests that need a GPU **skip** rather than fail when no adapter is available:
//! they are worth having where they can run, and a hard failure on a machine
//! without a usable GPU would say nothing about the code.

use openpaint_core::tile::{TILE_BYTES, TILE_SIZE};

use crate::canvas_renderer::{CanvasRenderer, CANVAS_FORMAT};
use crate::stroke_layer::StrokeLayer;
use crate::tile_store::LayerId;

/// The layer every single-layer test paints on.
pub const L0: LayerId = LayerId(0);

/// Surface format the GPU tests render to. sRGB, like the real one.
pub const SURFACE: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// Side length used by the GPU tests. 128 keeps them quick, and it is deliberately
/// **smaller than one tile**, so a page that does not fill its tiles is the default case
/// the comparisons run against.
pub const SIZE: u32 = 128;

/// A headless device, or `None` where there is no usable adapter.
pub fn try_device() -> Option<(wgpu::Device, wgpu::Queue)> {
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

/// A canvas renderer with a residency budget generous enough that tests exercising the
/// *paint* path are not also fighting eviction.
///
/// Tests that mean to exercise eviction build a [`crate::tile_store::TileStore`] with a
/// deliberately tiny budget instead.
pub fn test_canvas(
    device: &wgpu::Device,
    page: openpaint_core::PageRect,
    stroke: &StrokeLayer,
) -> CanvasRenderer {
    CanvasRenderer::new(device, SURFACE, page, 128 * 1024 * 1024, stroke)
}

/// A stroke layer for tests. Separate because the compositor needs it at construction.
pub fn test_stroke_layer(device: &wgpu::Device) -> StrokeLayer {
    StrokeLayer::new(device, CANVAS_FORMAT)
}

/// Read one layer's page region back as linear f32 RGBA, row-major.
///
/// Area with no tile reads as **transparent**, matching what a layer actually holds. This is a
/// layer, not a composited page: the paper belongs to the compositor, so a comparison against
/// the CPU reference (which now also produces a transparent layer) lines up directly.
pub fn readback_page(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    canvas: &CanvasRenderer,
    layer: LayerId,
) -> Vec<[f32; 4]> {
    let page = canvas.page();
    let (w, h) = (page.w as usize, page.h as usize);
    let mut out = vec![[0.0f32; 4]; w * h];

    let coords: Vec<openpaint_core::tile::TileCoord> = canvas
        .layer_tiles(layer)
        .filter(|c| crate::canvas_renderer::tile_intersects(*c, page))
        .collect();
    if coords.is_empty() {
        return out;
    }

    // A tile row is 256 × 8 = 2048 bytes and a whole tile 512 KiB, both already multiples
    // of the 256-byte copy alignment, so tiles pack end to end with no padding.
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: (TILE_BYTES * coords.len()) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&Default::default());
    for (i, coord) in coords.iter().enumerate() {
        // Loudly, not silently: a spilled tile would otherwise read back as paper and a
        // comparison against the CPU reference would fail for a reason that looks like a
        // rasterization bug.
        let slot = canvas
            .slot(layer, *coord)
            .unwrap_or_else(|| panic!("tile {coord:?} is not resident; raise the test budget"));
        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture: canvas.pool().texture(),
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: 0,
                    z: slot.layer(),
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyBuffer {
                buffer: &buffer,
                layout: wgpu::ImageDataLayout {
                    offset: (TILE_BYTES * i) as u64,
                    bytes_per_row: Some(TILE_SIZE as u32 * 8),
                    rows_per_image: Some(TILE_SIZE as u32),
                },
            },
            wgpu::Extent3d {
                width: TILE_SIZE as u32,
                height: TILE_SIZE as u32,
                depth_or_array_layers: 1,
            },
        );
    }
    queue.submit(std::iter::once(encoder.finish()));

    buffer.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    device.poll(wgpu::Maintain::Wait);
    let view = buffer.slice(..).get_mapped_range();
    let texels = bytemuck::cast_slice::<u8, half::f16>(&view)
        .as_chunks::<4>()
        .0;

    let t = TILE_SIZE as i32;
    for (i, coord) in coords.iter().enumerate() {
        let tile = &texels[i * TILE_SIZE * TILE_SIZE..(i + 1) * TILE_SIZE * TILE_SIZE];
        for ly in 0..TILE_SIZE {
            let py = coord.1 * t + ly as i32 - page.y;
            if py < 0 || py >= page.h as i32 {
                continue;
            }
            for lx in 0..TILE_SIZE {
                let px = coord.0 * t + lx as i32 - page.x;
                if px < 0 || px >= page.w as i32 {
                    continue;
                }
                let c = tile[ly * TILE_SIZE + lx];
                out[py as usize * w + px as usize] =
                    [c[0].to_f32(), c[1].to_f32(), c[2].to_f32(), c[3].to_f32()];
            }
        }
    }
    out
}

/// Read one resident tile back as linear f32 RGBA, row-major within the tile.
///
/// For tests where most of the canvas is deliberately spilled, so whole-page readback would
/// (correctly) refuse.
pub fn readback_tile(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    canvas: &CanvasRenderer,
    layer: LayerId,
    coord: openpaint_core::tile::TileCoord,
) -> Option<Vec<[f32; 4]>> {
    let slot = canvas.slot(layer, coord)?;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("tile-readback"),
        size: TILE_BYTES as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        wgpu::ImageCopyTexture {
            texture: canvas.pool().texture(),
            mip_level: 0,
            origin: wgpu::Origin3d {
                x: 0,
                y: 0,
                z: slot.layer(),
            },
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::ImageCopyBuffer {
            buffer: &buffer,
            layout: wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(TILE_SIZE as u32 * 8),
                rows_per_image: Some(TILE_SIZE as u32),
            },
        },
        wgpu::Extent3d {
            width: TILE_SIZE as u32,
            height: TILE_SIZE as u32,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(std::iter::once(encoder.finish()));
    buffer.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    device.poll(wgpu::Maintain::Wait);
    let view = buffer.slice(..).get_mapped_range();
    Some(
        bytemuck::cast_slice::<u8, half::f16>(&view)
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| [c[0].to_f32(), c[1].to_f32(), c[2].to_f32(), c[3].to_f32()])
            .collect(),
    )
}

/// Largest per-channel difference between two readbacks, and where it is.
pub fn max_difference(a: &[[f32; 4]], b: &[[f32; 4]]) -> (f32, usize) {
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
pub fn mean_difference(a: &[[f32; 4]], b: &[[f32; 4]]) -> f32 {
    let mut total = 0.0f64;
    for (p, q) in a.iter().zip(b) {
        for c in 0..4 {
            total += f64::from((p[c] - q[c]).abs());
        }
    }
    (total / (a.len() * 4) as f64) as f32
}

/// Whether any pixel carries coverage, i.e. paint actually landed.
///
/// Alpha, not colour: a layer starts transparent, so alpha is exactly how much paint is there.
/// Comparing against paper would only work on a composited page.
pub fn any_paint(pixels: &[[f32; 4]]) -> bool {
    pixels.iter().any(|p| p[3] > 0.05)
}
