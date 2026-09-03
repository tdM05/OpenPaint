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

/// Largest page a PNG export will attempt, in pixels.
///
/// A PNG is one flat image, so export has to materialise `w * h * 4` bytes on the CPU no
/// matter how sparse the canvas is. At 512 Mpx that is 2 GiB, which is the point past
/// which refusing is kinder than an allocation failure. Removing this needs a streaming
/// encoder that writes row bands as they are read — worth doing when a page that big is a
/// real workflow rather than a theoretical one.
const MAX_EXPORT_PIXELS: u64 = 512 * 1024 * 1024;

/// What an export is about to do.
///
/// Held by the shell between frames, like any other half-finished UI state, and shown in a modal
/// built from the same [`Control`](crate::panel_ui::Control)s every panel is built from.
///
/// **Kept after an export, not reset.** Somebody exporting a strip at 50% is going to do it again
/// in ten minutes, and a dialog that forgets is a dialog you have to set up every time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Choices {
    pub what: What,
    /// A percentage of the page's own size, 10 to 100.
    pub scale: u32,
}

impl Default for Choices {
    fn default() -> Self {
        Self {
            what: What::ThisPage,
            scale: FULL_SIZE,
        }
    }
}

/// How much of the document goes out, and in how many files.
///
/// **Three choices rather than "everything" plus a switch.** A switch would spell a fourth state
/// -- this page, as a strip -- which is the same thing as this page and would have to be either
/// hidden or explained. Three named intents cannot be combined into a meaningless one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum What {
    /// The page being drawn on.
    ThisPage,
    /// Every page, numbered, one file each.
    EveryPage,
    /// Every page stacked into one tall image: a webtoon.
    Strip,
}

impl What {
    /// What the artist chooses between, in the order it is offered.
    pub const ALL: [Self; 3] = [Self::ThisPage, Self::EveryPage, Self::Strip];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::ThisPage => "This page",
            Self::EveryPage => "Every page, one file each",
            Self::Strip => "Every page, one tall strip",
        }
    }
}

/// Control ids for the export modal.
///
/// Past the prompts' own (`crate::prompt`), because both are drawn by the same modal and an id
/// that meant two things would make the answer depend on which was asked first.
const WHAT_BASE: crate::panel_ui::ControlId = 100;
const SCALE: crate::panel_ui::ControlId = 110;

/// The smallest export offered, as a percentage.
///
/// Not zero, and not one: below about a tenth the page is a thumbnail, and every step of the
/// slider below that would be a size nobody asked for.
const MIN_SCALE: u32 = 10;

/// What the export modal shows, given what is chosen and how many pages there are.
#[must_use]
pub fn controls(
    choices: &Choices,
    pages: usize,
    page: (u32, u32),
) -> Vec<crate::panel_ui::Control> {
    use crate::panel_ui::Control;
    let mut controls = Vec::new();
    for (i, what) in What::ALL.iter().enumerate() {
        // **A document of one page is not offered three ways to export one page.** Two of them
        // would do exactly the same thing, and a choice with no consequence is a choice the
        // artist has to think about for nothing.
        if pages < 2 && *what != What::ThisPage {
            continue;
        }
        #[expect(clippy::cast_possible_truncation, reason = "three of them")]
        controls.push(Control::Choice {
            id: WHAT_BASE + i as u32,
            text: what.label().to_owned(),
            selected: choices.what == *what,
            icon: None,
        });
    }
    controls.push(Control::Slider {
        id: SCALE,
        // **Not "Size".** The Brush panel has a Size and so does Text, and the control atlas
        // refuses a name that fits three controls rather than guessing between them -- which is
        // the rule that stopped a scenario deleting the artist's saved brushes. A name is only
        // useful if it is a name.
        text: "Export size".to_owned(),
        value: choices.scale as f32,
        min: MIN_SCALE as f32,
        max: FULL_SIZE as f32,
        unit: "%",
        log: false,
    });
    // **The size it will actually be**, because a percentage is not a size and the number that
    // matters to a webtoon is a pixel width. Worked out by the same function that will do it, so
    // this cannot promise a size the export does not produce.
    let (w, h) = scaled_size(page, choices.scale);
    controls.push(Control::Label {
        text: match choices.what {
            What::ThisPage => format!("One file, {w} by {h} pixels."),
            What::EveryPage => format!("{pages} files, {w} by {h} pixels each."),
            What::Strip => format!("One file, {w} by {} pixels.", u64::from(h) * pages as u64),
        },
    });
    controls
}

/// Act on something changed in the export modal. Returns whether anything moved.
pub fn apply(choices: &mut Choices, change: &crate::panel_ui::Change) -> bool {
    use crate::panel_ui::Change;
    match *change {
        Change::Chose(id) if (WHAT_BASE..WHAT_BASE + What::ALL.len() as u32).contains(&id) => {
            let what = What::ALL[(id - WHAT_BASE) as usize];
            let moved = choices.what != what;
            choices.what = what;
            moved
        }
        Change::Set(SCALE, value) => {
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "clamped to 10..=100 by the slider before it arrives"
            )]
            let scale = (value.round() as u32).clamp(MIN_SCALE, FULL_SIZE);
            let moved = choices.scale != scale;
            choices.scale = scale;
            moved
        }
        _ => false,
    }
}

/// The files an export writes, given the name the artist chose.
///
/// One page keeps the name as given. Several are numbered from one, with the number *before* the
/// extension and zero-padded, so a folder of them sorts the way the document reads -- `page-9`
/// and `page-10` sort the wrong way round in every file manager there is.
#[must_use]
pub fn names(base: &Path, pages: usize) -> Vec<PathBuf> {
    if pages <= 1 {
        return vec![base.to_path_buf()];
    }
    let extension = base
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_else(|| "png".to_owned());
    let stem = base
        .file_stem()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_else(|| "page".to_owned());
    let width = pages.to_string().len();
    (1..=pages)
        .map(|n| {
            let name = format!("{stem}-{n:0width$}.{extension}", width = width);
            base.with_file_name(name)
        })
        .collect()
}

/// One page, flattened and ready to write: 8-bit sRGB with alpha, row-major.
///
/// Named because a strip is several of these stacked, and because the *scaled* size is not the
/// page's size — so passing the pixels around with their own dimensions is the only way the two
/// cannot come apart.
pub struct Sheet {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// A scale of 100%: the page at the size it was drawn.
pub const FULL_SIZE: u32 = 100;

impl Sheet {
    /// Write it as an sRGB PNG.
    ///
    /// # Errors
    /// Whatever the encoder or the filesystem said.
    pub fn write(&self, path: &Path) -> Result<(), ExportError> {
        write_png(path, self.width, self.height, &self.rgba)
    }

    /// Stack sheets into one tall image, each centred on the widest.
    ///
    /// **This is what a webtoon is.** The format is a single continuous strip a reader scrolls,
    /// not a folder of numbered files, and stitching one by hand in another program is the step
    /// that would make this application useless for the thing it was built for.
    ///
    /// Centred rather than left-aligned, and padded with paper rather than with transparency: a
    /// page narrower than its neighbours is a page, not a hole, and a reader scrolling past it
    /// should see the sheet continue.
    #[must_use]
    pub fn stack(sheets: &[Self]) -> Self {
        let width = sheets.iter().map(|s| s.width).max().unwrap_or(0);
        let height = sheets.iter().map(|s| s.height).sum();
        let paper = to_srgb8(&f16x4(Canvas::paper_color()));
        let mut rgba = Vec::with_capacity((width as usize) * (height as usize) * 4);
        for sheet in sheets {
            let left = (width - sheet.width) / 2;
            for y in 0..sheet.height {
                for _ in 0..left {
                    rgba.extend_from_slice(&paper);
                }
                let row = (y as usize) * (sheet.width as usize) * 4;
                rgba.extend_from_slice(&sheet.rgba[row..row + (sheet.width as usize) * 4]);
                for _ in 0..(width - sheet.width - left) {
                    rgba.extend_from_slice(&paper);
                }
            }
        }
        Self {
            width,
            height,
            rgba,
        }
    }
}

/// Flatten one page at a scale, into 8-bit sRGB.
///
/// `scale` is a percentage of the page's own size, and 100 is the page as drawn.
///
/// **The scale is applied while reading, not to a finished image.** Each output pixel averages
/// the block of page pixels it covers, straight out of the composited tiles — so exporting a
/// 2048-pixel page at a quarter size never materialises the full-size image at all, and the
/// average is taken in *linear* light where averaging means what it says. Downscaling in sRGB is
/// the classic way to make artwork come out darker than it is.
///
/// # Errors
/// [`ExportError::TooLarge`] when the result would not fit in memory as one image.
pub fn compose(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    canvas: &CanvasRenderer,
    layers: &[Layer],
    page: openpaint_core::PageRect,
    scale: u32,
) -> Result<Sheet, ExportError> {
    let (full_w, full_h) = (page.w.max(1), page.h.max(1));
    let (w, h) = scaled_size((full_w, full_h), scale);
    if u64::from(w) * u64::from(h) > MAX_EXPORT_PIXELS {
        return Err(ExportError::TooLarge { w, h });
    }

    let flat = flatten(device, queue, canvas, layers, page);
    let paper = Canvas::paper_color();
    let at = |px: i32, py: i32| {
        flat.get(&crate::canvas_renderer::tile_of(px, py))
            .map_or(paper, |tile| tile[local_index(px, py)])
    };
    let mut rgba = Vec::with_capacity((w as usize) * (h as usize) * 4);
    for y in 0..h {
        // The block of source rows this output row stands for. Worked out from the output index
        // both times, so consecutive rows meet exactly and no source row is used twice or missed.
        let (y0, y1) = span(y, h, full_h);
        for x in 0..w {
            let (x0, x1) = span(x, w, full_w);
            let mut sum = [0.0_f32; 4];
            let mut n = 0.0_f32;
            for py in y0..y1 {
                for px in x0..x1 {
                    let texel = at(page.x + px as i32, page.y + py as i32);
                    for (acc, v) in sum.iter_mut().zip(texel) {
                        *acc += v;
                    }
                    n += 1.0;
                }
            }
            // Through `f16` at the end, like every other texel that leaves the canvas: the
            // stored precision is what the screen showed, and an export that was a fraction more
            // precise than the picture it came from would differ from it for no visible reason.
            let mean = sum.map(|v| v / n.max(1.0));
            rgba.extend_from_slice(&to_srgb8(&f16x4(mean)));
        }
    }
    Ok(Sheet {
        width: w,
        height: h,
        rgba,
    })
}

/// The size a page comes out at, at a scale in percent. Never zero in either direction.
#[must_use]
pub fn scaled_size((w, h): (u32, u32), scale: u32) -> (u32, u32) {
    if scale == FULL_SIZE {
        return (w.max(1), h.max(1));
    }
    let of = |v: u32| {
        (u64::from(v) * u64::from(scale) / 100)
            .try_into()
            .unwrap_or(u32::MAX)
    };
    (of(w).max(1), of(h).max(1))
}

/// Which source pixels output index `i` of `out` covers, given `full` source pixels.
fn span(i: u32, out: u32, full: u32) -> (u32, u32) {
    let start = (u64::from(i) * u64::from(full) / u64::from(out.max(1))) as u32;
    let end = (u64::from(i + 1) * u64::from(full) / u64::from(out.max(1))) as u32;
    (start, end.max(start + 1).min(full))
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
    page: openpaint_core::PageRect,
) -> std::collections::HashMap<TileCoord, Vec<[f32; 4]>> {
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

/// One tile of a merge: `upper`'s pixels folded into `lower`'s, as the compositor already shows
/// them.
///
/// Lives here, beside [`Composite`], because it is the same rule seen from a different angle — a
/// merge must produce *what the artist was already looking at*, so it has to answer exactly as the
/// compositor does about opacity, blend and clipping. A second copy of that arithmetic anywhere is
/// a second idea of what a stack means, which is the mistake this module was extracted to undo.
///
/// `under` is `None` where the lower layer has no tile, which is transparent — not an error, since
/// a layer routinely has pixels its neighbour does not.
///
/// What deliberately does *not* happen here: the lower layer's own opacity and blend are not baked
/// in. They stay on the layer and go on applying to the merged result, which is what every app does
/// and is exact whenever it is Normal at full strength. Baking them would change how the merged
/// layer sits against everything *below* it, which is a bigger lie than the one it fixes.
#[must_use]
pub(crate) fn merge_tile(
    src: &openpaint_core::tile::Tile,
    under: Option<&openpaint_core::tile::Tile>,
    upper: &Layer,
    lower: &Layer,
) -> openpaint_core::tile::Tile {
    let mut out = openpaint_core::tile::Tile::transparent();
    for i in 0..openpaint_core::tile::TILE_TEXELS {
        let (x, y) = (i % TILE_SIZE, i / TILE_SIZE);
        let base = under.map_or([0.0; 4], |t| t.texel(x, y));
        // The upper layer's contribution: its own opacity, then its clip to the layer below if it
        // has one. Exactly `Composite::add`, for one layer over one backdrop.
        let mut over =
            openpaint_core::color::scale_premul(src.texel(x, y), upper.effective_opacity());
        if upper.clip_below {
            // The base's *contribution* alpha, not its raw pixel alpha -- the same rule
            // `Composite::add` uses, so that fading the base fades what is clipped to it.
            over = openpaint_core::color::scale_premul(over, base[3] * lower.effective_opacity());
        }
        out.set_texel(x, y, blend_over(over, base, upper.blend));
    }
    out
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
mod choice_tests {
    use super::*;
    use crate::panel_ui::{Change, Control};

    fn labels(controls: &[Control]) -> Vec<String> {
        controls
            .iter()
            .filter_map(|c| match c {
                Control::Choice { text, .. } | Control::Label { text } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    /// One page is named as the artist named it; several are numbered, and sort in reading order.
    ///
    /// **The padding is the point.** `page-9` and `page-10` sort the wrong way round in every file
    /// manager and in every upload form that takes a folder, which turns a correctly exported
    /// comic into a shuffled one — and the artist would find out from a reader.
    #[test]
    fn several_pages_are_numbered_so_they_sort_in_order() {
        let base = Path::new("C:/art/chapter.png");
        assert_eq!(names(base, 1), vec![PathBuf::from("C:/art/chapter.png")]);
        assert_eq!(
            names(base, 3),
            vec![
                PathBuf::from("C:/art/chapter-1.png"),
                PathBuf::from("C:/art/chapter-2.png"),
                PathBuf::from("C:/art/chapter-3.png"),
            ]
        );
        let ten = names(base, 10);
        assert_eq!(ten.first().unwrap(), Path::new("C:/art/chapter-01.png"));
        assert_eq!(ten.last().unwrap(), Path::new("C:/art/chapter-10.png"));
        // Sorted as text, they are still in page order -- which is the whole reason for the zero.
        let mut sorted: Vec<String> = ten.iter().map(|p| p.display().to_string()).collect();
        let asked = sorted.clone();
        sorted.sort();
        assert_eq!(sorted, asked, "numbered files do not sort into page order");
    }

    /// A name with no extension still writes PNGs.
    #[test]
    fn a_name_without_an_extension_still_gets_one() {
        assert_eq!(
            names(Path::new("C:/art/chapter"), 2),
            vec![
                PathBuf::from("C:/art/chapter-1.png"),
                PathBuf::from("C:/art/chapter-2.png"),
            ]
        );
    }

    /// Scaling never produces a zero-pixel side, however small the page or the percentage.
    ///
    /// A zero-width PNG is not a small file, it is an invalid one — and the encoder would be the
    /// thing that reported it, several layers away from the slider that caused it.
    #[test]
    fn a_scale_never_shrinks_a_side_to_nothing() {
        assert_eq!(scaled_size((1200, 1600), FULL_SIZE), (1200, 1600));
        assert_eq!(scaled_size((1200, 1600), 50), (600, 800));
        assert_eq!(scaled_size((1200, 1600), 10), (120, 160));
        assert_eq!(scaled_size((3, 1), 10), (1, 1));
    }

    /// Every output pixel of a scaled export covers source pixels, and covers each exactly once.
    ///
    /// The two ways to get this wrong are a gap — a row of the drawing that reaches no output
    /// pixel and simply vanishes — and an overlap, which counts the same ink twice and shows up
    /// as banding. Both are invisible in a thumbnail and obvious in the finished page.
    #[test]
    fn the_blocks_a_scale_averages_tile_the_page_exactly() {
        for (full, out) in [(1600_u32, 800_u32), (1600, 160), (1000, 333), (7, 3)] {
            let mut covered = vec![0_u32; full as usize];
            for i in 0..out {
                let (a, b) = span(i, out, full);
                assert!(b > a, "output pixel {i} of {out} averages nothing");
                for p in a..b {
                    covered[p as usize] += 1;
                }
            }
            assert!(
                covered.iter().all(|n| *n == 1),
                "{full} into {out}: some source pixels were used {:?} times",
                {
                    let mut seen: Vec<u32> = covered.clone();
                    seen.sort_unstable();
                    seen.dedup();
                    seen
                }
            );
        }
    }

    /// A document of one page is not offered three ways to export one page.
    #[test]
    fn one_page_is_offered_one_way_out() {
        let choices = Choices::default();
        let said = labels(&controls(&choices, 1, (1200, 1600)));
        assert!(
            said.iter().any(|s| s == What::ThisPage.label()),
            "the only page cannot be exported: {said:?}"
        );
        for hidden in [What::EveryPage, What::Strip] {
            assert!(
                !said.iter().any(|s| s == hidden.label()),
                "a one-page document was offered {said:?}"
            );
        }
    }

    /// With pages to choose between, all three are offered and the size line follows the choice.
    #[test]
    fn the_size_line_says_what_will_actually_be_written() {
        let mut choices = Choices::default();
        let of = |c: &Choices| {
            labels(&controls(c, 3, (1200, 1600)))
                .last()
                .cloned()
                .expect("a size line")
        };
        assert!(of(&choices).contains("1200 by 1600"), "{}", of(&choices));

        choices.what = What::EveryPage;
        assert!(of(&choices).contains('3'), "{}", of(&choices));

        // A strip is as tall as its pages put together, and that is the number a webtoon platform
        // asks for.
        choices.what = What::Strip;
        assert!(of(&choices).contains("4800"), "{}", of(&choices));

        // And the scale reaches it.
        choices.scale = 50;
        assert!(of(&choices).contains("600 by 2400"), "{}", of(&choices));
    }

    /// The dialog's controls answer to the dialog, and say when nothing moved.
    #[test]
    fn a_change_moves_exactly_what_it_names() {
        let mut choices = Choices::default();
        let id = |what: What| {
            controls(&Choices::default(), 3, (10, 10))
                .into_iter()
                .find_map(|c| match c {
                    Control::Choice { id, text, .. } if text == what.label() => Some(id),
                    _ => None,
                })
                .expect("a choice for every way out")
        };
        assert!(apply(&mut choices, &Change::Chose(id(What::Strip))));
        assert_eq!(choices.what, What::Strip);
        // The same choice again changes nothing, and says so -- a frame that redraws for a press
        // that moved nothing is a frame spent for nothing.
        assert!(!apply(&mut choices, &Change::Chose(id(What::Strip))));

        assert!(apply(&mut choices, &Change::Set(SCALE, 42.4)));
        assert_eq!(choices.scale, 42);
        // Out of range is clamped rather than obeyed: a zero-percent export has no pixels in it.
        assert!(apply(&mut choices, &Change::Set(SCALE, -5.0)));
        assert_eq!(choices.scale, MIN_SCALE);
        assert!(apply(&mut choices, &Change::Set(SCALE, 400.0)));
        assert_eq!(choices.scale, FULL_SIZE);

        // Something from another panel entirely is not this dialog's to act on.
        let before = choices;
        assert!(!apply(&mut choices, &Change::Pressed(7)));
        assert!(!apply(&mut choices, &Change::Toggled(WHAT_BASE, true)));
        assert_eq!(choices, before);
    }

    /// A strip is as tall as its pages together, as wide as the widest, and centres the rest.
    #[test]
    fn a_strip_stacks_its_pages_and_centres_the_narrow_ones() {
        let sheet = |w: u32, h: u32, fill: u8| Sheet {
            width: w,
            height: h,
            rgba: vec![fill; (w * h * 4) as usize],
        };
        let strip = Sheet::stack(&[sheet(4, 2, 11), sheet(2, 1, 22)]);
        assert_eq!((strip.width, strip.height), (4, 3));
        assert_eq!(strip.rgba.len(), (4 * 3 * 4) as usize);
        // The first page fills its rows.
        assert!(strip.rgba[..32].iter().all(|b| *b == 11));
        // The second is two pixels of it with one of paper either side, not four of it and not
        // two of it flush against the left edge.
        let paper = to_srgb8(&f16x4(Canvas::paper_color()));
        let last = &strip.rgba[32..];
        assert_eq!(
            &last[0..4],
            &paper,
            "a narrow page was not padded on the left"
        );
        assert!(
            last[4..12].iter().all(|b| *b == 22),
            "the page itself moved"
        );
        assert_eq!(
            &last[12..16],
            &paper,
            "a narrow page was not padded on the right"
        );
    }
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

        let stroke = crate::test_gpu::test_stroke_layer(device);
        let mut canvas = crate::test_gpu::test_canvas(device, page, &stroke);
        let mut enc = device.create_command_encoder(&Default::default());
        canvas.upload_dirty(device, queue, &mut enc, crate::test_gpu::L0, &mut cpu);
        queue.submit(std::iter::once(enc.finish()));
        let doc = openpaint_core::Page::new(page.w, page.h);

        let path = std::env::temp_dir().join("openpaint-export-tiles-test.png");
        compose(
            device,
            queue,
            &canvas,
            doc.layers(),
            canvas.page(),
            FULL_SIZE,
        )
        .expect("compose failed")
        .write(&path)
        .expect("export failed");

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
        let stroke = crate::test_gpu::test_stroke_layer(device);
        let canvas = crate::test_gpu::test_canvas(device, doc.rect(), &stroke);
        assert_eq!(
            canvas.occupied_tiles().len(),
            0,
            "nothing should be resident yet"
        );

        let path = std::env::temp_dir().join("openpaint-export-blank-test.png");
        compose(
            device,
            queue,
            &canvas,
            doc.layers(),
            canvas.page(),
            FULL_SIZE,
        )
        .expect("compose failed")
        .write(&path)
        .expect("export failed");

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
