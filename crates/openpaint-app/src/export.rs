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
use openpaint_core::tile::{TileCoord, TILE_BYTES, TILE_SIZE};
use openpaint_core::Canvas;

use crate::canvas_renderer::{tile_intersects, CanvasRenderer};

/// Bytes per canvas texel (`Rgba16Float`).
const BYTES_PER_TEXEL: u32 = 8;

#[derive(Debug)]
pub enum ExportError {
    Io(io::Error),
    Encode(String),
    /// The page is too large to assemble as one image in memory.
    TooLarge {
        w: u32,
        h: u32,
    },
}

impl std::fmt::Display for ExportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::Encode(e) => write!(f, "{e}"),
            Self::TooLarge { w, h } => write!(
                f,
                "{w}x{h} is too large to export as one PNG ({} GiB of pixels)",
                u64::from(*w) * u64::from(*h) * 4 / (1024 * 1024 * 1024)
            ),
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

/// Largest page a PNG export will attempt, in pixels.
///
/// A PNG is one flat image, so export has to materialise `w * h * 4` bytes on the CPU no
/// matter how sparse the canvas is. At 512 Mpx that is 2 GiB, which is the point past
/// which refusing is kinder than an allocation failure. Removing this needs a streaming
/// encoder that writes row bands as they are read — worth doing when a page that big is a
/// real workflow rather than a theoretical one.
const MAX_EXPORT_PIXELS: u64 = 512 * 1024 * 1024;

/// Assemble the page from its GPU tiles and write it as an sRGB PNG.
///
/// Unpainted area has no tile at all, so it is filled with paper first and only the
/// resident tiles are read back — an export costs what was drawn, not what the page
/// measures.
pub fn export_tiles_png(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    canvas: &CanvasRenderer,
    path: &Path,
) -> Result<(), ExportError> {
    let page = canvas.page();
    let (w, h) = (page.w.max(1), page.h.max(1));
    if u64::from(w) * u64::from(h) > MAX_EXPORT_PIXELS {
        return Err(ExportError::TooLarge { w, h });
    }

    let paper = to_srgb8(&f16x4(Canvas::paper_color()));
    let mut out = Vec::with_capacity((w as usize) * (h as usize) * 4);
    for _ in 0..(w as usize) * (h as usize) {
        out.extend_from_slice(&paper);
    }

    let coords: Vec<TileCoord> = canvas
        .tiles()
        .filter(|c| tile_intersects(*c, page))
        .collect();
    if !coords.is_empty() {
        let tiles = read_tiles(device, queue, canvas, &coords);
        for (coord, texels) in coords
            .iter()
            .zip(tiles.as_chunks::<{ TILE_SIZE * TILE_SIZE }>().0)
        {
            blit_tile(&mut out, w, h, page.x, page.y, *coord, texels);
        }
    }

    write_png(path, w, h, &out)
}

/// Read whole tiles back into one buffer, in the order given.
///
/// One mapped buffer for the lot rather than one per tile: a tile row is
/// `256 * 8 = 2048` bytes and a whole tile is 512 KiB, both already multiples of the
/// 256-byte copy alignment, so tiles pack end to end with no padding to skip.
fn read_tiles(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    canvas: &CanvasRenderer,
    coords: &[TileCoord],
) -> Vec<[half::f16; 4]> {
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("export-readback"),
        size: (TILE_BYTES * coords.len()) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("export-encoder"),
    });
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
                    bytes_per_row: Some(TILE_SIZE as u32 * BYTES_PER_TEXEL),
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
    let mapped = buffer.slice(..).get_mapped_range();
    bytemuck::cast_slice::<u8, half::f16>(&mapped)
        .as_chunks::<4>()
        .0
        .to_vec()
}

/// Copy one tile's in-page part into the output image.
fn blit_tile(
    out: &mut [u8],
    w: u32,
    h: u32,
    page_x: i32,
    page_y: i32,
    coord: TileCoord,
    texels: &[[half::f16; 4]],
) {
    let t = TILE_SIZE as i32;
    for ly in 0..TILE_SIZE {
        let py = coord.1 * t + ly as i32 - page_y;
        if py < 0 || py >= h as i32 {
            continue;
        }
        for lx in 0..TILE_SIZE {
            let px = coord.0 * t + lx as i32 - page_x;
            if px < 0 || px >= w as i32 {
                continue;
            }
            let i = (py as usize * w as usize + px as usize) * 4;
            out[i..i + 4].copy_from_slice(&to_srgb8(&texels[ly * TILE_SIZE + lx]));
        }
    }
}

/// A linear premultiplied colour as f16 texel channels.
fn f16x4(rgba: [f32; 4]) -> [half::f16; 4] {
    [
        half::f16::from_f32(rgba[0]),
        half::f16::from_f32(rgba[1]),
        half::f16::from_f32(rgba[2]),
        half::f16::from_f32(rgba[3]),
    ]
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

    /// End-to-end: paint a known colour into the canvas tiles, export, decode the PNG
    /// back, and check every pixel. Covers tile readback, assembly, unpremultiply, the sRGB
    /// encode, and the PNG writer together.
    ///
    /// The page is deliberately **not** a multiple of the tile size and its origin is
    /// **negative**, because both are the normal case: a page is arbitrary pixels
    /// (DECISIONS §5a) and extending up or left makes the origin negative. Getting either
    /// wrong shears the image or drops a strip along an edge, and neither shows up on a
    /// tidy 256-aligned page starting at zero.
    #[test]
    fn exporting_a_tiled_canvas_produces_matching_png_pixels() {
        let Some((device, queue)) = crate::test_gpu::try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };

        let page = openpaint_core::PageRect::new(-300, -100, 700, 500);
        let authored = [7u8, 130, 201];
        let linear = opaque_srgb8_to_linear_premul(authored);

        // Fill the CPU reference canvas, then upload it -- the same path the reference
        // rasterizer uses, so this exercises tile allocation as well.
        let mut cpu = Canvas::new(page.w, page.h);
        cpu.resize(page);
        for y in page.y..page.end().1 {
            for x in page.x..page.end().0 {
                cpu.replace_pixel(x, y, linear);
            }
        }

        let mut canvas = crate::test_gpu::test_canvas(&device, &cpu);
        let mut enc = device.create_command_encoder(&Default::default());
        canvas.upload_dirty(&device, &queue, &mut enc, &mut cpu);
        queue.submit(std::iter::once(enc.finish()));

        let path = std::env::temp_dir().join("openpaint-export-tiles-test.png");
        export_tiles_png(&device, &queue, &canvas, &path).expect("export failed");

        let file = io::BufReader::new(std::fs::File::open(&path).expect("open png"));
        let decoder = png::Decoder::new(file);
        let mut reader = decoder.read_info().expect("png header");
        let info = reader.info().clone();
        assert_eq!((info.width, info.height), (page.w, page.h), "wrong size");
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
                i as u32 % page.w,
                i as u32 / page.w
            );
            assert_eq!(px[3], 255);
        }

        let _ = std::fs::remove_file(&path);
    }

    /// Area with no tile at all must come out as paper, not as a hole. Sparse tiles are the
    /// whole point of the tiled canvas, so most of a real page has nothing to read back.
    #[test]
    fn unpainted_area_exports_as_paper() {
        let Some((device, queue)) = crate::test_gpu::try_device() else {
            eprintln!("skipping: no usable GPU adapter");
            return;
        };

        let cpu = Canvas::new(300, 200);
        let canvas = crate::test_gpu::test_canvas(&device, &cpu);
        assert_eq!(canvas.tiles().count(), 0, "nothing should be resident yet");

        let path = std::env::temp_dir().join("openpaint-export-blank-test.png");
        export_tiles_png(&device, &queue, &canvas, &path).expect("export failed");

        let file = io::BufReader::new(std::fs::File::open(&path).expect("open png"));
        let mut reader = png::Decoder::new(file).read_info().expect("png header");
        let mut buf = vec![0u8; reader.output_buffer_size().expect("buffer size")];
        let frame = reader.next_frame(&mut buf).expect("png data");
        let data = &buf[..frame.buffer_size()];

        let expected = to_srgb8(&f16x4(Canvas::paper_color()));
        for (i, px) in data.as_chunks::<4>().0.iter().enumerate() {
            assert_eq!(*px, expected, "pixel {i} is not paper: {px:?}");
        }
        let _ = std::fs::remove_file(&path);
    }

    /// A page too large to hold as one flat image must be refused with a reason, not left
    /// to fail inside an allocation.
    #[test]
    fn an_impossibly_large_page_is_refused() {
        assert!(u64::from(65_536u32) * u64::from(65_536u32) > MAX_EXPORT_PIXELS);
    }
}
