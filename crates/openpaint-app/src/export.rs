//! PNG export — reading the canvas back off the GPU and writing a file.
//!
//! Deliberately *only* PNG for now, not the native format. `.openpaint` is
//! conceptually a zip of tiles plus JSON (DECISIONS §7), but its specifics (Q6)
//! should be settled **after** the document/page model exists. Defining it now
//! would define it for one fixed-size page, and then pages would force a v2 plus a
//! migration path for files nobody has yet. PNG has no such problem: it is a flat
//! image with nothing to design.
//!
//! # Colour conversion is the part worth getting right
//!
//! The canvas is linear, premultiplied `Rgba16Float` (DECISIONS §4b). A PNG is
//! sRGB-encoded, straight-alpha, 8-bit. So export must:
//!
//! 1. **un**premultiply (divide by alpha), then
//! 2. encode linear → sRGB through the real transfer function.
//!
//! Skipping step 2 is the classic mistake and produces a visibly washed-out file —
//! the same double/missing-encode error that was already fixed once in the renderer.
//! Both directions live in `openpaint_core::color`, so there is one implementation.
//!
//! # Readback stalls, on purpose
//!
//! Mapping a buffer means waiting for the GPU. That is a hitch, and it is fine here:
//! export is an explicit user action, not something on the drawing path. The paint
//! path deliberately never reads back (see `crate::history`).

use std::io;
use std::path::{Path, PathBuf};

use openpaint_core::color::linear_to_srgb8;

/// Bytes per canvas texel (`Rgba16Float`).
const BYTES_PER_TEXEL: u32 = 8;

#[derive(Debug)]
pub enum ExportError {
    Io(io::Error),
    Encode(String),
}

impl std::fmt::Display for ExportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::Encode(e) => write!(f, "{e}"),
        }
    }
}

impl From<io::Error> for ExportError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// A default filename in the current directory, distinct per second.
///
/// No file dialog yet: that arrives with real save/open, where it belongs. Seconds
/// since the epoch rather than a formatted date to avoid a date-formatting
/// dependency for a placeholder.
#[must_use]
pub fn default_path() -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    PathBuf::from(format!("openpaint-{stamp}.png"))
}

/// Read the canvas texture back and write it as an sRGB PNG.
///
/// `texture` must be `Rgba16Float` with `COPY_SRC`, and `width`/`height` must match
/// it.
pub fn export_canvas_png(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    width: u32,
    height: u32,
    path: &Path,
) -> Result<(), ExportError> {
    let pixels = read_texture(device, queue, texture, width, height);
    write_png(path, width, height, &pixels)
}

/// Read the texture into straight-alpha sRGB bytes, RGBA order.
fn read_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    width: u32,
    height: u32,
) -> Vec<u8> {
    // Buffer rows must be a multiple of 256 bytes, which the canvas width usually
    // satisfies but a growable canvas will not. Pad, then skip the padding on read.
    let unpadded = width * BYTES_PER_TEXEL;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded = unpadded.div_ceil(align) * align;

    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("export-readback"),
        size: u64::from(padded) * u64::from(height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("export-encoder"),
    });
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
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(std::iter::once(encoder.finish()));

    buffer.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    device.poll(wgpu::Maintain::Wait);

    let mapped = buffer.slice(..).get_mapped_range();
    let mut out = Vec::with_capacity((width * height * 4) as usize);
    for row in 0..height {
        let start = (row * padded) as usize;
        let end = start + unpadded as usize;
        let halves: &[half::f16] = bytemuck::cast_slice(&mapped[start..end]);
        for texel in halves.as_chunks::<4>().0 {
            out.extend_from_slice(&to_srgb8(texel));
        }
    }
    out
}

/// Convert one linear premultiplied texel to straight-alpha sRGB bytes.
fn to_srgb8(texel: &[half::f16; 4]) -> [u8; 4] {
    let a = texel[3].to_f32().clamp(0.0, 1.0);
    // Unpremultiply. At a == 0 the colour carries no information, so emit
    // transparent black rather than dividing by zero.
    let unpremul = |c: half::f16| if a > 0.0 { c.to_f32() / a } else { 0.0 };
    [
        linear_to_srgb8(unpremul(texel[0])),
        linear_to_srgb8(unpremul(texel[1])),
        linear_to_srgb8(unpremul(texel[2])),
        (a * 255.0).round() as u8,
    ]
}

fn write_png(path: &Path, width: u32, height: u32, rgba: &[u8]) -> Result<(), ExportError> {
    let file = std::fs::File::create(path)?;
    let writer = io::BufWriter::new(file);
    let mut encoder = png::Encoder::new(writer, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    // Tag the file as sRGB so viewers don't guess. Perceptual is the right intent
    // for artwork.
    encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);

    let mut writer = encoder
        .write_header()
        .map_err(|e| ExportError::Encode(e.to_string()))?;
    writer
        .write_image_data(rgba)
        .map_err(|e| ExportError::Encode(e.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use openpaint_core::color::opaque_srgb8_to_linear_premul;

    fn texel(rgba: [f32; 4]) -> [half::f16; 4] {
        [
            half::f16::from_f32(rgba[0]),
            half::f16::from_f32(rgba[1]),
            half::f16::from_f32(rgba[2]),
            half::f16::from_f32(rgba[3]),
        ]
    }

    /// An authored colour must survive the whole trip: sRGB in, linear on the
    /// canvas, sRGB back out. If this drifts, exports look washed out -- the same
    /// class of error that was already fixed once in the renderer.
    #[test]
    fn authored_colours_round_trip_to_png_bytes() {
        for rgb in [
            [0, 0, 0],
            [255, 255, 255],
            [20, 20, 24],
            [250, 249, 246],
            [7, 130, 201],
        ] {
            let linear = opaque_srgb8_to_linear_premul(rgb);
            let out = to_srgb8(&texel(linear));
            assert_eq!(
                [out[0], out[1], out[2]],
                rgb,
                "round trip failed for {rgb:?}"
            );
            assert_eq!(out[3], 255, "opaque texel should export opaque");
        }
    }

    /// Mid grey must not come out at 128 in *linear* -- that would mean the sRGB
    /// encode was skipped, the classic washed-out export.
    #[test]
    fn linear_mid_grey_is_not_exported_as_mid_grey() {
        let out = to_srgb8(&texel([0.5, 0.5, 0.5, 1.0]));
        assert!(
            out[0] > 180,
            "linear 0.5 should encode to ~188 in sRGB, got {}",
            out[0]
        );
    }

    #[test]
    fn transparent_texels_do_not_divide_by_zero() {
        let out = to_srgb8(&texel([0.0, 0.0, 0.0, 0.0]));
        assert_eq!(out, [0, 0, 0, 0]);
    }

    /// Premultiplied input must be undone, or semi-transparent paint exports too
    /// dark.
    #[test]
    fn premultiplied_colour_is_undone() {
        // Half-transparent white, premultiplied: colour channels are 0.5.
        let out = to_srgb8(&texel([0.5, 0.5, 0.5, 0.5]));
        assert_eq!([out[0], out[1], out[2]], [255, 255, 255], "got {out:?}");
        assert!((i32::from(out[3]) - 128).abs() <= 1);
    }

    #[test]
    fn default_path_is_a_png_in_the_current_directory() {
        let p = default_path();
        assert_eq!(p.extension().and_then(|e| e.to_str()), Some("png"));
        assert!(p.parent().is_none_or(|d| d.as_os_str().is_empty()));
    }

    /// End-to-end: fill a texture with a known colour, export, decode the PNG back,
    /// and check every pixel. Covers readback, unpremultiply, sRGB encode, and the
    /// PNG writer together.
    ///
    /// Uses a width whose row is **not** 256-byte aligned (100 × 8 = 800 bytes),
    /// because that is the case a growable canvas will hit constantly and the one
    /// where forgetting to skip row padding silently shears the image.
    #[test]
    fn exporting_a_known_canvas_produces_matching_png_pixels() {
        let Some((device, queue)) = crate::test_gpu::try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };

        const W: u32 = 100;
        const H: u32 = 37;
        let authored = [7u8, 130, 201];

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("export-test"),
            size: wgpu::Extent3d {
                width: W,
                height: H,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: crate::canvas_renderer::CANVAS_FORMAT,
            usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });

        let linear = opaque_srgb8_to_linear_premul(authored);
        let one: Vec<half::f16> = linear.iter().map(|c| half::f16::from_f32(*c)).collect();
        let filled: Vec<half::f16> = one.repeat((W * H) as usize);
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
                bytes_per_row: Some(W * BYTES_PER_TEXEL),
                rows_per_image: Some(H),
            },
            wgpu::Extent3d {
                width: W,
                height: H,
                depth_or_array_layers: 1,
            },
        );

        let path = std::env::temp_dir().join("openpaint-export-test.png");
        export_canvas_png(&device, &queue, &texture, W, H, &path).expect("export failed");

        // Decode it back and check every pixel, so row padding can't hide.
        let file = io::BufReader::new(std::fs::File::open(&path).expect("open png"));
        let decoder = png::Decoder::new(file);
        let mut reader = decoder.read_info().expect("png header");
        let info = reader.info().clone();
        assert_eq!((info.width, info.height), (W, H), "wrong dimensions");
        assert_eq!(info.color_type, png::ColorType::Rgba);

        let mut buf = vec![0u8; reader.output_buffer_size().expect("buffer size")];
        let frame = reader.next_frame(&mut buf).expect("png data");
        let data = &buf[..frame.buffer_size()];

        for (i, px) in data.as_chunks::<4>().0.iter().enumerate() {
            assert_eq!(
                [px[0], px[1], px[2]],
                authored,
                "pixel {} ({}, {}) wrong: {px:?}",
                i,
                i as u32 % W,
                i as u32 / W
            );
            assert_eq!(px[3], 255);
        }

        let _ = std::fs::remove_file(&path);
    }
}
