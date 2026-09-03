//! Reading a picture that is not ours.
//!
//! A comics page usually starts life somewhere else — a pencil sketch on paper, photographed or
//! scanned; a panel roughed out on a phone; a reference someone sent. Until this existed, `Open`
//! read `.openpaint` and nothing else, so none of that could get in at all.
//!
//! # What comes out
//!
//! [`Picture`] is **straight** (non-premultiplied) 8-bit sRGB with alpha — the form every image
//! format hands over and the form nothing inside this application uses. The conversion to the
//! canvas's linear premultiplied `f16` happens in [`Picture::tiles`], in one place, so a decoder
//! added later cannot invent a second answer to "what do these bytes mean".
//!
//! # Two formats, not a library of them
//!
//! PNG because it is lossless and what a drawing is exported as; JPEG because it is what a camera
//! and a scanner produce. `image` would bring both and ten more, and `DECISIONS` already argued
//! the small surface for the export side — the same argument holds coming in.
//!
//! # The kind sniffs the bytes, not the name
//!
//! A phone writes `.jpg`; a person renames it `.png` when a website asks for one; the bytes never
//! change. Deciding by extension would refuse a perfectly good picture, and say something untrue
//! about why.

use openpaint_core::tile::{Tile, TileCoord, TILE_SIZE};
use std::collections::HashMap;
use std::path::Path;

/// The first bytes of a PNG file, which the format guarantees.
const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";

/// The start of a JPEG file: the JFIF/EXIF start-of-image marker.
const JPEG_MAGIC: &[u8] = b"\xff\xd8\xff";

/// A decoded picture, in straight 8-bit sRGB with alpha.
///
/// Row-major, four bytes per pixel, `width * height * 4` long — checked on construction, because
/// everything downstream indexes into it and a short buffer would be a panic in a loop rather
/// than a message about a file.
#[derive(Clone, PartialEq, Eq)]
pub struct Picture {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

/// Why a picture could not be used.
///
/// Each variant is something a person can act on: find another file, save it as something else,
/// scale it down. "Failed to decode" is not a reason, it is a shrug.
#[derive(Debug)]
pub enum Trouble {
    /// The file could not be read at all.
    Unreadable(std::io::Error),
    /// The bytes are neither a PNG nor a JPEG.
    NotAPicture,
    /// It is one of those, and it is damaged.
    Damaged(String),
    /// Larger than a page can be. See [`crate::editor::MAX_PAGE_DIMENSION`].
    TooBig { width: u32, height: u32 },
}

impl std::fmt::Display for Trouble {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreadable(e) => write!(f, "the file could not be read ({e})"),
            Self::NotAPicture => write!(f, "that is not a PNG or a JPEG"),
            Self::Damaged(why) => write!(f, "the image is damaged ({why})"),
            Self::TooBig { width, height } => write!(
                f,
                "{width}x{height} is larger than a page can be ({} either way)",
                crate::editor::MAX_PAGE_DIMENSION
            ),
        }
    }
}

/// Read a picture from disk.
pub fn read(path: &Path) -> Result<Picture, Trouble> {
    let bytes = std::fs::read(path).map_err(Trouble::Unreadable)?;
    decode(&bytes)
}

/// Decode a picture from bytes, deciding the format from the bytes themselves.
pub fn decode(bytes: &[u8]) -> Result<Picture, Trouble> {
    if !sniff(bytes) {
        return Err(Trouble::NotAPicture);
    }
    if bytes.starts_with(PNG_MAGIC) {
        decode_png(bytes)
    } else {
        decode_jpeg(bytes)
    }
}

/// Whether these opening bytes are a picture this module can read.
///
/// Separate from [`decode`] so a caller can ask about the first few bytes of a file without
/// reading all of it -- and so the answer comes from the same two constants the decoders are
/// chosen by, rather than from a second list that could disagree with them.
#[must_use]
pub fn sniff(head: &[u8]) -> bool {
    head.starts_with(PNG_MAGIC) || head.starts_with(JPEG_MAGIC)
}

/// The file extensions worth offering in a dialog.
///
/// Only a hint to the picker — [`decode`] still reads the bytes, so a mislabelled file that gets
/// through the filter is opened rather than refused for having the wrong name.
pub const EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "jpe", "jfif"];

fn decode_png(bytes: &[u8]) -> Result<Picture, Trouble> {
    // A `Cursor`, because the decoder wants to seek: an interlaced PNG is read in passes and an
    // ancillary chunk is skipped by jumping over it.
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    // **Every PNG comes out as 8-bit RGBA**, whatever it was: palette, greyscale, 16-bit, with or
    // without transparency. The alternative is a match over five colour types times two bit
    // depths written here, which is the decoder's job and which it already does correctly.
    decoder.set_transformations(
        png::Transformations::normalize_to_color8() | png::Transformations::ALPHA,
    );
    let mut reader = decoder
        .read_info()
        .map_err(|e| Trouble::Damaged(e.to_string()))?;
    // `None` means the size does not fit in a `usize`, which on a 64-bit machine means a header
    // claiming something no disk holds -- damaged, not merely large.
    let size = reader
        .output_buffer_size()
        .ok_or_else(|| Trouble::Damaged("a size nothing could hold".to_owned()))?;
    let mut buffer = vec![0; size];
    let info = reader
        .next_frame(&mut buffer)
        .map_err(|e| Trouble::Damaged(e.to_string()))?;
    buffer.truncate(info.buffer_size());
    // **Two arms, because the transformations above leave exactly two possibilities**: colour of
    // any depth or palette arrives as RGBA, and greyscale arrives as grey plus alpha. There were
    // four arms here, and the two extra ones read as careful handling of the other colour types
    // while being unreachable -- a sabotage that filled `Rgb` with transparent black changed
    // nothing and no test noticed. Code that cannot run is not caution, it is furniture.
    let rgba = match info.color_type {
        png::ColorType::Rgba => buffer,
        png::ColorType::GrayscaleAlpha => widen(&buffer, 2, |p| [p[0], p[0], p[0], p[1]]),
        other => {
            return Err(Trouble::Damaged(format!(
                "the decoder produced {other:?}, which the transformations asked it not to"
            )))
        }
    };
    Picture::new(info.width, info.height, rgba)
}

fn decode_jpeg(bytes: &[u8]) -> Result<Picture, Trouble> {
    let mut decoder = jpeg_decoder::Decoder::new(bytes);
    let pixels = decoder
        .decode()
        .map_err(|e| Trouble::Damaged(e.to_string()))?;
    let info = decoder
        .info()
        .ok_or_else(|| Trouble::Damaged("no header".to_owned()))?;
    // A JPEG has no alpha; it is a photograph of something, and every pixel of it is there.
    let rgba = match info.pixel_format {
        jpeg_decoder::PixelFormat::RGB24 => widen(&pixels, 3, |p| [p[0], p[1], p[2], 255]),
        jpeg_decoder::PixelFormat::L8 => widen(&pixels, 1, |p| [p[0], p[0], p[0], 255]),
        jpeg_decoder::PixelFormat::L16 => {
            // Sixteen bits a channel, little-endian, and the top eight are all a canvas of 8-bit
            // sRGB can hold anyway.
            widen(&pixels, 2, |p| [p[1], p[1], p[1], 255])
        }
        jpeg_decoder::PixelFormat::CMYK32 => {
            return Err(Trouble::Damaged(
                "it is a CMYK JPEG, which needs a colour profile to convert honestly".to_owned(),
            ))
        }
    };
    Picture::new(u32::from(info.width), u32::from(info.height), rgba)
}

/// Expand `stride`-channel pixels into RGBA, one call per pixel.
fn widen(src: &[u8], stride: usize, to_rgba: impl Fn(&[u8]) -> [u8; 4]) -> Vec<u8> {
    let mut out = Vec::with_capacity(src.len() / stride * 4);
    for pixel in src.chunks_exact(stride) {
        out.extend_from_slice(&to_rgba(pixel));
    }
    out
}

impl Picture {
    /// Build a picture, checking that it is a size a page could hold and that the buffer is the
    /// size the dimensions claim.
    ///
    /// **Both checks here rather than at each decoder**, because both are about what this
    /// application can hold rather than about any format -- and a check per decoder is a check
    /// the next decoder forgets.
    pub fn new(width: u32, height: u32, rgba: Vec<u8>) -> Result<Self, Trouble> {
        let max = crate::editor::MAX_PAGE_DIMENSION;
        if width > max || height > max {
            return Err(Trouble::TooBig { width, height });
        }
        let wanted = (width as usize)
            .checked_mul(height as usize)
            .and_then(|n| n.checked_mul(4));
        if width == 0 || height == 0 || wanted != Some(rgba.len()) {
            return Err(Trouble::Damaged(format!(
                "{width}x{height} needs {} bytes and there are {}",
                wanted.unwrap_or(0),
                rgba.len()
            )));
        }
        Ok(Self {
            width,
            height,
            rgba,
        })
    }

    #[must_use]
    pub fn width(&self) -> u32 {
        self.width
    }

    #[must_use]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// The pixels, straight sRGB with alpha, row-major.
    ///
    /// Borrowed rather than copied: the clipboard hands this straight to the platform, and a
    /// clipboard copy of a full-page selection is not a buffer to duplicate for the sake of it.
    #[must_use]
    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }

    /// Build a picture from linear premultiplied texels -- the canvas's own form.
    ///
    /// The way *out*, and the exact inverse of [`straight_srgb8_to_linear_premul`]: unpremultiply,
    /// then encode through the transfer function. Doing those two in the other order is the
    /// classic mistake and shows as a dark fringe on every soft edge -- and the reason this lives
    /// beside its inverse is so the pair can be read together and tested against each other.
    ///
    /// `at` is asked for every pixel of the `width` by `height` rectangle, in page coordinates
    /// offset by `origin`.
    #[must_use]
    pub fn from_texels(
        width: u32,
        height: u32,
        origin: (i32, i32),
        at: impl Fn(i32, i32) -> [f32; 4],
    ) -> Option<Self> {
        use openpaint_core::color::linear_to_srgb8;
        let mut rgba = Vec::with_capacity((width as usize) * (height as usize) * 4);
        for y in 0..height {
            for x in 0..width {
                #[expect(clippy::cast_possible_wrap, reason = "bounded by MAX_PAGE_DIMENSION")]
                let texel = at(origin.0 + x as i32, origin.1 + y as i32);
                let a = texel[3].clamp(0.0, 1.0);
                let straight = |c: f32| if a > 0.0 { c / a } else { 0.0 };
                rgba.extend_from_slice(&[
                    linear_to_srgb8(straight(texel[0])),
                    linear_to_srgb8(straight(texel[1])),
                    linear_to_srgb8(straight(texel[2])),
                    #[expect(
                        clippy::cast_possible_truncation,
                        clippy::cast_sign_loss,
                        reason = "clamped to 0..=1 above"
                    )]
                    {
                        (a * 255.0).round() as u8
                    },
                ]);
            }
        }
        Self::new(width, height, rgba).ok()
    }

    /// One pixel, as straight sRGB with alpha.
    #[must_use]
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let at = ((y as usize * self.width as usize) + x as usize) * 4;
        Some([
            self.rgba[at],
            self.rgba[at + 1],
            self.rgba[at + 2],
            self.rgba[at + 3],
        ])
    }

    /// The picture as canvas tiles, with its top-left corner at `at` in page pixels.
    ///
    /// **This is the conversion, and it is here once.** Straight sRGB in, linear premultiplied
    /// `f16` out — the canvas's own format (§4b). Doing it per decoder would be three chances to
    /// get the premultiply the wrong way round, which shows up as a halo round every soft edge
    /// and is invisible until someone looks at a feathered PNG on a dark background.
    ///
    /// Only tiles the picture actually touches are produced, and a tile it half-covers keeps the
    /// rest transparent — so placing a small scan on a big page costs the scan, not the page.
    #[must_use]
    pub fn tiles(&self, at: (i32, i32)) -> HashMap<TileCoord, Tile> {
        let size = TILE_SIZE as i32;
        // `div_euclid`, not a plain division: a picture placed at a negative offset -- which is
        // what centring something larger than the page does -- must land in tile -1, not tile 0.
        let first = (at.0.div_euclid(size), at.1.div_euclid(size));
        #[expect(
            clippy::cast_possible_wrap,
            reason = "checked against MAX_PAGE_DIMENSION, far inside i32"
        )]
        let last = (
            (at.0 + self.width as i32 - 1).div_euclid(size),
            (at.1 + self.height as i32 - 1).div_euclid(size),
        );
        let mut out = HashMap::new();
        for ty in first.1..=last.1 {
            for tx in first.0..=last.0 {
                let mut tile = Tile::transparent();
                let mut touched = false;
                for y in 0..TILE_SIZE {
                    for x in 0..TILE_SIZE {
                        #[expect(
                            clippy::cast_possible_truncation,
                            clippy::cast_possible_wrap,
                            reason = "a tile is 128 across"
                        )]
                        let page = (tx * size + x as i32, ty * size + y as i32);
                        let (px, py) = (page.0 - at.0, page.1 - at.1);
                        if px < 0 || py < 0 {
                            continue;
                        }
                        #[expect(
                            clippy::cast_sign_loss,
                            reason = "both are known non-negative here"
                        )]
                        let Some(rgba) = self.pixel(px as u32, py as u32) else {
                            continue;
                        };
                        if rgba[3] == 0 {
                            continue;
                        }
                        tile.set_texel(x, y, straight_srgb8_to_linear_premul(rgba));
                        touched = true;
                    }
                }
                // A tile the picture reaches but leaves entirely transparent is not a tile: it
                // would cost a pool slot and composite as nothing.
                if touched {
                    out.insert((tx, ty), tile);
                }
            }
        }
        out
    }
}

/// Straight 8-bit sRGB with alpha to linear premultiplied — the canvas's texel.
///
/// **Premultiply after the transfer function, never before.** Multiplying the sRGB value by alpha
/// and converting afterwards is wrong by exactly the curve, and it is wrong most where alpha is
/// low — which is every anti-aliased edge in the picture.
fn straight_srgb8_to_linear_premul(rgba: [u8; 4]) -> [f32; 4] {
    use openpaint_core::color::srgb8_to_linear;
    let a = f32::from(rgba[3]) / 255.0;
    [
        srgb8_to_linear(rgba[0]) * a,
        srgb8_to_linear(rgba[1]) * a,
        srgb8_to_linear(rgba[2]) * a,
        a,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny PNG, encoded by the same crate that reads it.
    fn png_of(width: u32, height: u32, rgba: &[u8], colour: png::ColorType) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, width, height);
            encoder.set_color(colour);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().expect("a header");
            writer.write_image_data(rgba).expect("the pixels");
        }
        out
    }

    #[test]
    fn a_png_with_alpha_comes_back_as_it_went_in() {
        let pixels = [255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 255, 0, 9, 9, 9, 255];
        let file = png_of(2, 2, &pixels, png::ColorType::Rgba);
        let picture = decode(&file).expect("a picture");
        assert_eq!((picture.width(), picture.height()), (2, 2));
        assert_eq!(picture.pixel(0, 0), Some([255, 0, 0, 255]));
        assert_eq!(picture.pixel(1, 0), Some([0, 255, 0, 128]));
        assert_eq!(picture.pixel(1, 1), Some([9, 9, 9, 255]));
    }

    /// A PNG with no alpha channel is opaque, not transparent.
    ///
    /// The obvious mistake is to leave alpha at whatever the buffer happened to hold, which for a
    /// zeroed buffer is an invisible picture — and "I imported my sketch and nothing appeared" is
    /// indistinguishable from a broken importer.
    #[test]
    fn a_png_without_alpha_is_opaque() {
        let file = png_of(2, 1, &[10, 20, 30, 40, 50, 60], png::ColorType::Rgb);
        let picture = decode(&file).expect("a picture");
        assert_eq!(picture.pixel(0, 0), Some([10, 20, 30, 255]));
        assert_eq!(picture.pixel(1, 0), Some([40, 50, 60, 255]));
    }

    /// Greyscale is a colour type too, and a scanned pencil sketch is very often exactly that.
    #[test]
    fn a_greyscale_png_becomes_grey_rgb() {
        let file = png_of(2, 1, &[0, 200], png::ColorType::Grayscale);
        let picture = decode(&file).expect("a picture");
        assert_eq!(picture.pixel(0, 0), Some([0, 0, 0, 255]));
        assert_eq!(picture.pixel(1, 0), Some([200, 200, 200, 255]));
    }

    /// The format is decided by the bytes, so a JPEG called `.png` still opens.
    #[test]
    fn the_name_does_not_decide_the_format() {
        let file = png_of(1, 1, &[1, 2, 3, 255], png::ColorType::Rgba);
        // `decode` is never told the name at all, which is the point: there is nowhere for an
        // extension to be consulted.
        assert!(decode(&file).is_ok());
        assert!(matches!(
            decode(b"GIF89a and then some"),
            Err(Trouble::NotAPicture)
        ));
    }

    /// A damaged file is refused with a reason, never half-decoded into noise.
    #[test]
    fn a_damaged_png_says_so() {
        let mut file = png_of(4, 4, &[7; 64], png::ColorType::Rgba);
        // Keep the magic, ruin the rest: this is what a truncated download looks like.
        file.truncate(PNG_MAGIC.len() + 8);
        assert!(matches!(decode(&file), Err(Trouble::Damaged(_))));
    }

    /// Placed at the origin, the top-left pixel is the top-left texel.
    #[test]
    fn a_picture_lands_where_it_is_put() {
        let file = png_of(
            2,
            1,
            &[255, 0, 0, 255, 0, 0, 255, 255],
            png::ColorType::Rgba,
        );
        let picture = decode(&file).expect("a picture");

        let tiles = picture.tiles((0, 0));
        assert_eq!(tiles.len(), 1, "two pixels cannot need two tiles");
        let tile = tiles.get(&(0, 0)).expect("the first tile");
        assert!(tile.texel(0, 0)[0] > 0.9, "red is not where it was put");
        assert!(tile.texel(1, 0)[2] > 0.9, "blue is not where it was put");
        assert_eq!(tile.texel(2, 0), [0.0; 4], "the rest is not transparent");

        // And moved, it moves. Written against `TILE_SIZE` rather than against a number that
        // happens to be inside the second tile today: the tile size is a tuning decision in
        // `openpaint_core`, and a test that hardcodes a multiple of it fails the day it changes
        // while saying nothing about placement.
        let at = TILE_SIZE as i32 + 2;
        let moved = picture.tiles((at, 0));
        assert_eq!(moved.keys().copied().collect::<Vec<_>>(), vec![(1, 0)]);
        let tile = moved.get(&(1, 0)).expect("the second tile");
        assert!(
            tile.texel(2, 0)[0] > 0.9,
            "red did not move with the picture"
        );
    }

    /// **Premultiplied, and premultiplied in linear light.**
    ///
    /// Half-transparent mid-grey is the case that separates the three plausible orderings: premul
    /// before the transfer function gives 0.216, after it gives 0.108, and forgetting to premul at
    /// all gives 0.216 in the colour with 0.5 in alpha. Only one of those is right, and the wrong
    /// ones look like a halo round every soft edge rather than like a bug.
    #[test]
    fn alpha_is_multiplied_in_linear_light() {
        let file = png_of(1, 1, &[188, 188, 188, 128], png::ColorType::Rgba);
        let picture = decode(&file).expect("a picture");
        let tiles = picture.tiles((0, 0));
        let texel = tiles.get(&(0, 0)).expect("the tile").texel(0, 0);
        let alpha = 128.0 / 255.0;
        let linear = openpaint_core::color::srgb8_to_linear(188);
        assert!(
            (texel[3] - alpha).abs() < 0.002,
            "alpha came out {}",
            texel[3]
        );
        assert!(
            (texel[0] - linear * alpha).abs() < 0.002,
            "the colour came out {} rather than {}",
            texel[0],
            linear * alpha
        );
    }

    /// A picture spanning tiles is cut along the tile grid, and nothing is lost at the seam.
    #[test]
    fn a_wide_picture_is_cut_into_tiles() {
        let (w, h) = (TILE_SIZE as u32 + 3, 2);
        let picture = Picture::new(w, h, vec![255; (w * h * 4) as usize]).expect("a picture");
        let tiles = picture.tiles((0, 0));
        let mut coords: Vec<TileCoord> = tiles.keys().copied().collect();
        coords.sort_unstable();
        assert_eq!(coords, vec![(0, 0), (1, 0)]);
        // The last three columns landed in the second tile, and the fourth is empty.
        let second = tiles.get(&(1, 0)).expect("the second tile");
        assert!(second.texel(2, 0)[3] > 0.9, "the seam lost a column");
        assert_eq!(
            second.texel(3, 0),
            [0.0; 4],
            "the picture grew past its end"
        );
    }

    /// Placed off the top-left corner, it lands in negative tiles rather than folding onto zero.
    ///
    /// Centring a picture wider than the page is exactly this case, and integer division truncates
    /// toward zero — so -1 and -127 would both have said tile 0 and stacked two different parts of
    /// the picture on top of each other.
    #[test]
    fn a_picture_placed_off_the_corner_lands_in_negative_tiles() {
        let picture = Picture::new(4, 1, vec![255; 16]).expect("a picture");
        let tiles = picture.tiles((-2, -1));
        let mut coords: Vec<TileCoord> = tiles.keys().copied().collect();
        coords.sort_unstable();
        assert_eq!(coords, vec![(-1, -1), (0, -1)]);
        let left = tiles.get(&(-1, -1)).expect("the tile off the edge");
        assert!(
            left.texel(TILE_SIZE - 2, TILE_SIZE - 1)[3] > 0.9,
            "the part off the page is not in the tile off the page"
        );
    }

    /// Bigger than a page can be is refused, and says the size rather than "too big".
    ///
    /// Checked through `Picture::new` rather than by encoding a 65537-pixel PNG, which would be a
    /// gigabyte of test for a comparison -- but through the same door every decoder comes out of,
    /// so a decoder cannot skip it.
    #[test]
    fn a_picture_too_big_for_a_page_is_refused() {
        let big = crate::editor::MAX_PAGE_DIMENSION + 1;
        let refused = Picture::new(big, 1, Vec::new());
        assert!(
            matches!(refused, Err(Trouble::TooBig { width, .. }) if width == big),
            "a picture wider than a page was not refused for being wide"
        );
        assert!(Trouble::TooBig {
            width: big,
            height: 1
        }
        .to_string()
        .contains(&big.to_string()));
    }

    /// Out and back again changes nothing that eight bits can hold.
    ///
    /// **The two conversions are written apart and have to agree.** One premultiplies after the
    /// transfer function; the other unpremultiplies before its inverse. Getting either the wrong
    /// way round survives a glance -- it is exact at alpha 1, and every test picture is opaque --
    /// and shows up as a fringe on soft edges in artwork somebody has already spent a day on.
    #[test]
    fn a_picture_survives_the_trip_through_the_canvas_and_back() {
        let pixels: Vec<u8> = vec![
            255, 0, 0, 255, // opaque, where the two orderings agree
            188, 188, 188, 128, // half transparent, where they do not
            0, 0, 0, 0, // nothing at all
            9, 200, 60, 40, // barely there, the worst case for the ordering
        ];
        let there = Picture::new(4, 1, pixels.clone()).expect("a picture");
        let tiles = there.tiles((0, 0));
        let tile = tiles.get(&(0, 0)).expect("the tile");
        #[expect(clippy::cast_sign_loss, reason = "the test's own small coordinates")]
        let back = Picture::from_texels(4, 1, (0, 0), |x, y| tile.texel(x as usize, y as usize))
            .expect("a picture");
        for i in 0..4 {
            let (was, now) = (
                there.pixel(i, 0).expect("a pixel"),
                back.pixel(i, 0).expect("a pixel"),
            );
            // Transparent black carries no colour, so only its alpha is meaningful.
            if was[3] == 0 {
                assert_eq!(now[3], 0, "transparent came back at alpha {}", now[3]);
                continue;
            }
            assert_eq!(was[3], now[3], "alpha moved at pixel {i}");
            for c in 0..3 {
                assert!(
                    was[c].abs_diff(now[c]) <= 2,
                    "pixel {i} channel {c} went out as {} and came back as {}",
                    was[c],
                    now[c]
                );
            }
        }
    }

    /// An empty or mis-sized buffer is refused rather than indexed into.
    #[test]
    fn a_buffer_that_does_not_match_its_size_is_refused() {
        assert!(matches!(
            Picture::new(2, 2, vec![0; 8]),
            Err(Trouble::Damaged(_))
        ));
        assert!(matches!(
            Picture::new(0, 4, Vec::new()),
            Err(Trouble::Damaged(_))
        ));
    }
}
