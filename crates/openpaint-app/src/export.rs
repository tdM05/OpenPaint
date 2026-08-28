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
use openpaint_core::{Blend, Canvas, Layer};

use crate::canvas_renderer::{tile_intersects, CanvasRenderer};
use crate::tile_store::LayerId;

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

/// Flatten the layer stack and write it as an sRGB PNG.
///
/// Composites on the **CPU**, through `openpaint_core::layer::Blend` — the same functions the
/// shader mirrors. Two reasons, and neither is convenience:
///
/// 1. Export already stalls on a readback, so there is nothing to gain from doing it on the
///    GPU, and doing it here needs no render target at all — which matters, because a
///    page-sized target is exactly the ceiling the tiled canvas removed.
/// 2. It makes the two compositing implementations comparable. A test that composites the same
///    stack through both and diffs the pixels is what stops the shader and the reference
///    drifting apart, the same way the dab tests pin the falloff curve.
pub fn export_tiles_png(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    canvas: &CanvasRenderer,
    layers: &[Layer],
    path: &Path,
) -> Result<(), ExportError> {
    let page = canvas.page();
    let (w, h) = (page.w.max(1), page.h.max(1));
    if u64::from(w) * u64::from(h) > MAX_EXPORT_PIXELS {
        return Err(ExportError::TooLarge { w, h });
    }

    let flat = flatten(device, queue, canvas, layers);
    let mut out = Vec::with_capacity((w as usize) * (h as usize) * 4);
    let paper = Canvas::paper_color();
    for y in 0..h {
        for x in 0..w {
            let px = page.x + x as i32;
            let py = page.y + y as i32;
            let texel = flat
                .get(&crate::canvas_renderer::tile_of(px, py))
                .map_or(paper, |tile| tile[local_index(px, py)]);
            out.extend_from_slice(&to_srgb8(&f16x4(texel)));
        }
    }
    write_png(path, w, h, &out)
}

/// Index of a page pixel within its tile.
fn local_index(px: i32, py: i32) -> usize {
    let t = TILE_SIZE as i32;
    let lx = px.rem_euclid(t) as usize;
    let ly = py.rem_euclid(t) as usize;
    ly * TILE_SIZE + lx
}

/// Composite the stack, tile by tile, into linear premultiplied f32 texels.
///
/// Keyed by tile so a sparse canvas costs only what was drawn, and so nothing page-sized is
/// ever allocated on the GPU side.
fn flatten(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    canvas: &CanvasRenderer,
    layers: &[Layer],
) -> std::collections::HashMap<TileCoord, Vec<[f32; 4]>> {
    let page = canvas.page();
    let mut out = std::collections::HashMap::new();
    let paper = Canvas::paper_color();

    let coords: Vec<TileCoord> = canvas
        .occupied_tiles()
        .into_iter()
        .filter(|c| tile_intersects(*c, page))
        .collect();

    for coord in coords {
        let mut acc: Vec<Composite> = (0..TILE_SIZE * TILE_SIZE)
            .map(|_| Composite::new(paper))
            .collect();
        for layer in layers {
            // Every layer, even hidden or unpainted ones. `Composite::add` needs to see them to
            // keep a clip group masked against the right base; a zero texel says "contributes
            // nothing", which is exactly true.
            let tile = if layer.effective_opacity() > 0.0 {
                read_tile(device, queue, canvas, LayerId(layer.id()), coord)
            } else {
                None
            };
            match tile {
                Some(src) => {
                    for (dst, s) in acc.iter_mut().zip(src) {
                        dst.add(s, layer);
                    }
                }
                None => {
                    for dst in &mut acc {
                        dst.add([0.0; 4], layer);
                    }
                }
            }
        }
        out.insert(coord, acc.into_iter().map(Composite::finish).collect());
    }
    out
}

/// Folds one pixel's worth of layer samples into the colour that pixel shows.
///
/// **The compositing rule, once.** Order, layer opacity, clipping to the layer below, and the blend
/// itself. It had begun to exist in three places — `composite_fs` in canvas.wgsl, the PNG export,
/// and the eyedropper's sampler — and clipping would have made that three separate ideas of what a
/// stack means. The WGSL copy is unavoidable and is pinned to this one by
/// `the_gpu_compositor_matches_the_cpu_reference`; the CPU copies are not, so there is now one.
///
/// Driven bottom-up: [`Composite::add`] once per layer in document order, then [`Composite::finish`].
pub(crate) struct Composite {
    out: [f32; 4],
    /// Contribution alpha of the most recent unclipped layer — what a clipped layer is masked by.
    base_alpha: f32,
}

impl Composite {
    /// Start from the paper, which is part of what is on screen.
    pub(crate) fn new(paper: [f32; 4]) -> Self {
        Self {
            out: paper,
            // Zero, so a clipped layer with nothing beneath it shows nothing rather than
            // everything. "Clip to the layer below" with no layer below has no other honest answer.
            base_alpha: 0.0,
        }
    }

    /// Add one layer's premultiplied tile texel, in bottom-up order.
    ///
    /// Call it for *every* layer including hidden and unpainted ones (a zero texel is fine): a
    /// skipped layer would leave `base_alpha` describing some earlier layer, so a clip group would
    /// silently mask against the wrong shape.
    pub(crate) fn add(&mut self, texel: [f32; 4], layer: &Layer) {
        // Premultiplied, so one multiply scales colour and coverage together.
        let mut src = openpaint_core::color::scale_premul(texel, layer.effective_opacity());
        if layer.clip_below {
            src = openpaint_core::color::scale_premul(src, self.base_alpha);
        } else {
            // The base's *contribution* — pixel alpha times layer opacity — not its raw pixel
            // alpha. One rule, from which both useful behaviours fall out rather than being
            // special-cased: hiding the base hides the group, and fading the base fades the group.
            self.base_alpha = src[3];
        }
        self.out = blend_over(src, self.out, layer.blend);
    }

    pub(crate) fn finish(self) -> [f32; 4] {
        self.out
    }
}

/// Composite premultiplied `src` over premultiplied `dst`, the PDF/CSS way.
///
/// Mirrors `blend_over` in canvas.wgsl. Blend functions are defined on straight colour, so this
/// un-premultiplies, blends, and recombines.
pub(crate) fn blend_over(src: [f32; 4], dst: [f32; 4], mode: Blend) -> [f32; 4] {
    let (sa, da) = (src[3], dst[3]);
    if sa <= 0.0 {
        return dst;
    }
    let mut out = [0.0f32; 4];
    for c in 0..3 {
        let cs = src[c] / sa;
        let cb = if da > 0.0 { dst[c] / da } else { 0.0 };
        let b = mode.apply(cs, cb);
        out[c] = sa * (1.0 - da) * cs + sa * da * b + (1.0 - sa) * da * cb;
    }
    out[3] = sa + da * (1.0 - sa);
    out
}

/// Read one layer's tile back as linear premultiplied f32, or `None` if it has none there.
fn read_tile(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    canvas: &CanvasRenderer,
    layer: LayerId,
    coord: TileCoord,
) -> Option<Vec<[f32; 4]>> {
    let slot = canvas.slot(layer, coord)?;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("export-readback"),
        size: TILE_BYTES as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("export-encoder"),
    });
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
                // A tile row is 256 x 8 = 2048 bytes, already a multiple of the 256-byte copy
                // alignment, so there is no padding to skip.
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

/// A linear premultiplied colour as f16 texel channels.
fn f16x4(rgba: [f32; 4]) -> [half::f16; 4] {
    [
        half::f16::from_f32(rgba[0]),
        half::f16::from_f32(rgba[1]),
        half::f16::from_f32(rgba[2]),
        half::f16::from_f32(rgba[3]),
    ]
}

/// The sRGB encode, for the test that compares the GPU compositor against this one.
#[cfg(test)]
pub(crate) fn to_srgb8_for_test(rgba: [f32; 4]) -> [u8; 4] {
    to_srgb8(&f16x4(rgba))
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
    /// A layer with `clip_below` set and nothing unclipped beneath it has nothing to clip to.
    ///
    /// The initial `base_alpha` is what decides this, and a GPU test whose bottom layer is unclipped
    /// never exercises it — the bottom layer overwrites the initial value before anything reads it.
    #[test]
    fn a_clipped_layer_at_the_bottom_shows_nothing() {
        let mut layer = Layer::restored(1, "clipped", 1.0, Blend::Normal, true, false, true);
        layer.clip_below = true;

        let mut fold = Composite::new([0.0; 4]);
        fold.add([1.0, 1.0, 1.0, 1.0], &layer);
        assert_eq!(
            fold.finish(),
            [0.0; 4],
            "clipping to a layer that is not there should show nothing, not everything"
        );
    }

    /// Consecutive clipped layers all clip to the same base, not to each other.
    ///
    /// This is what makes shading *and* highlights sit over one set of flats. A "clip to the layer
    /// directly below" implementation is indistinguishable until the second clipped layer exists,
    /// and then it leaks: the first clipped layer is solid, so it becomes an unrestricted base for
    /// the next one.
    #[test]
    fn consecutive_clipped_layers_share_one_base() {
        let base = Layer::restored(1, "base", 1.0, Blend::Normal, true, false, false);
        let mut first = Layer::restored(2, "shading", 1.0, Blend::Normal, true, false, true);
        let mut second = Layer::restored(3, "highlights", 1.0, Blend::Normal, true, false, true);
        first.clip_below = true;
        second.clip_below = true;

        // A base with *no* coverage here, and two solid clipped layers above it.
        let mut fold = Composite::new([0.0; 4]);
        fold.add([0.0; 4], &base);
        fold.add([0.0, 1.0, 0.0, 1.0], &first);
        fold.add([0.0, 0.0, 1.0, 1.0], &second);
        assert_eq!(
            fold.finish(),
            [0.0; 4],
            "the second clipped layer clipped to its neighbour rather than to the group's base"
        );
    }

    /// The mask is the base's alpha, not a boolean.
    #[test]
    fn clipping_is_weighted_by_the_base_alpha() {
        let base = Layer::restored(1, "base", 1.0, Blend::Normal, true, false, false);
        let mut over = Layer::restored(2, "over", 1.0, Blend::Normal, true, false, true);
        over.clip_below = true;

        let mut half = Composite::new([0.0; 4]);
        half.add([0.5, 0.0, 0.0, 0.5], &base);
        half.add([0.0, 0.0, 1.0, 1.0], &over);
        let half = half.finish();

        let mut full = Composite::new([0.0; 4]);
        full.add([1.0, 0.0, 0.0, 1.0], &base);
        full.add([0.0, 0.0, 1.0, 1.0], &over);
        let full = full.finish();

        // Read the *blue* channel, which only the clipped layer contributes. Composite alpha would
        // be the wrong measure: it includes the base's own coverage, so it rises even when the clip
        // is doing nothing.
        assert!(
            (half[2] - 0.5).abs() < 1e-3,
            "a half-covered base should half-show the clip: {half:?}"
        );
        assert!(
            full[2] > 0.99,
            "an opaque base should fully show it: {full:?}"
        );
    }

    /// Fading or hiding the base carries the clip group with it.
    ///
    /// One rule -- the mask is the base's *contribution*, alpha times layer opacity -- from which
    /// both behaviours fall out. Pinned because the tempting alternative (mask on raw pixel alpha)
    /// leaves shading floating over flats that have been hidden to look at something underneath.
    #[test]
    fn the_clip_group_follows_the_base_opacity() {
        let mut base = Layer::restored(1, "base", 1.0, Blend::Normal, true, false, false);
        let mut over = Layer::restored(2, "over", 1.0, Blend::Normal, true, false, true);
        over.clip_below = true;

        base.opacity = 0.5;
        let mut faded = Composite::new([0.0; 4]);
        faded.add([1.0, 0.0, 0.0, 1.0], &base);
        faded.add([0.0, 0.0, 1.0, 1.0], &over);
        let faded = faded.finish();
        assert!(
            (faded[2] - 0.5).abs() < 1e-3,
            "fading the base should fade the group: {faded:?}"
        );

        base.opacity = 1.0;
        base.visible = false;
        let mut hidden = Composite::new([0.0; 4]);
        hidden.add([1.0, 0.0, 0.0, 1.0], &base);
        hidden.add([0.0, 0.0, 1.0, 1.0], &over);
        assert_eq!(
            hidden.finish(),
            [0.0; 4],
            "hiding the base should hide what is clipped to it"
        );
    }

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

        let stroke = crate::test_gpu::test_stroke_layer(&device);
        let mut canvas = crate::test_gpu::test_canvas(&device, page, &stroke);
        let mut enc = device.create_command_encoder(&Default::default());
        canvas.upload_dirty(&device, &queue, &mut enc, crate::test_gpu::L0, &mut cpu);
        queue.submit(std::iter::once(enc.finish()));
        let doc = openpaint_core::Page::new(page.w, page.h);

        let path = std::env::temp_dir().join("openpaint-export-tiles-test.png");
        export_tiles_png(&device, &queue, &canvas, doc.layers(), &path).expect("export failed");

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

        let doc = openpaint_core::Page::new(300, 200);
        let stroke = crate::test_gpu::test_stroke_layer(&device);
        let canvas = crate::test_gpu::test_canvas(&device, doc.rect(), &stroke);
        assert_eq!(
            canvas.occupied_tiles().len(),
            0,
            "nothing should be resident yet"
        );

        let path = std::env::temp_dir().join("openpaint-export-blank-test.png");
        export_tiles_png(&device, &queue, &canvas, doc.layers(), &path).expect("export failed");

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
