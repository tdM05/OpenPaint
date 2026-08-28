//! Shared plumbing for the GPU tests.
//!
//! Exists so `stroke_layer` and `history` can both test against a real device
//! without duplicating device setup and texture readback. Test-only.
//!
//! Tests that need a GPU **skip** rather than fail when no adapter is available:
//! they are worth having where they can run, and a hard failure on a machine
//! without a usable GPU would say nothing about the code.

use openpaint_core::tile::{TILE_BYTES, TILE_SIZE};
use openpaint_core::Canvas;

use crate::canvas_renderer::CanvasRenderer;

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

/// Read the page region of a tiled canvas back as linear f32 RGBA, row-major.
///
/// Area with no resident tile reads as paper, matching what the sheet quad draws — so a
/// comparison against the CPU reference (which allocates tiles the same way) lines up
/// without either side having to know which tiles happen to exist.
pub fn readback_page(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    canvas: &CanvasRenderer,
) -> Vec<[f32; 4]> {
    let page = canvas.page();
    let (w, h) = (page.w as usize, page.h as usize);
    let paper = Canvas::paper_color().map(|c| half::f16::from_f32(c).to_f32());
    let mut out = vec![paper; w * h];

    let coords: Vec<openpaint_core::tile::TileCoord> = canvas
        .tiles()
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
        let Some(slot) = canvas.slot(*coord) else {
            continue;
        };
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

/// Whether any pixel differs meaningfully from paper, i.e. paint actually landed.
pub fn any_paint(pixels: &[[f32; 4]]) -> bool {
    let paper = Canvas::paper_color();
    pixels.iter().any(|p| (p[0] - paper[0]).abs() > 0.05)
}
