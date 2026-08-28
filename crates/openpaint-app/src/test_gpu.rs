//! Shared plumbing for the GPU tests.
//!
//! Exists so `stroke_layer` and `history` can both test against a real device
//! without duplicating device setup and texture readback. Test-only.
//!
//! Tests that need a GPU **skip** rather than fail when no adapter is available:
//! they are worth having where they can run, and a hard failure on a machine
//! without a usable GPU would say nothing about the code.

use openpaint_core::Canvas;

use crate::canvas_renderer::CANVAS_FORMAT;

/// Side length used by the GPU tests. 128 keeps them quick, and 128 × 8 bytes is a
/// 1024-byte row, satisfying wgpu's 256-byte `bytes_per_row` alignment for readback.
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

/// A canvas texture filled with paper, matching what the real renderer starts from.
pub fn make_canvas(device: &wgpu::Device, queue: &wgpu::Queue) -> wgpu::Texture {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("test-canvas"),
        size: wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: CANVAS_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::COPY_DST
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });

    let paper = Canvas::paper_color();
    let texel: Vec<half::f16> = paper.iter().map(|c| half::f16::from_f32(*c)).collect();
    let filled: Vec<half::f16> = texel.repeat((SIZE * SIZE) as usize);
    queue.write_texture(
        wgpu::ImageCopyTexture {
            texture: &texture,
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
    texture
}

/// Read a `SIZE` × `SIZE` canvas texture back as linear f32 RGBA.
pub fn readback(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
) -> Vec<[f32; 4]> {
    let bytes = (SIZE * SIZE * 8) as wgpu::BufferAddress;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: bytes,
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

    buffer.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    device.poll(wgpu::Maintain::Wait);
    let view = buffer.slice(..).get_mapped_range();
    let halves: &[half::f16] = bytemuck::cast_slice(&view);
    halves
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| [c[0].to_f32(), c[1].to_f32(), c[2].to_f32(), c[3].to_f32()])
        .collect()
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
