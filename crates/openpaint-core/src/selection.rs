//! Selection — a per-pixel coverage mask over the document.
//!
//! # One primitive, several consumers
//!
//! A selection is not a tool. It is a **mask**: how much each pixel is selected, from none to
//! fully. Producing one and consuming one are separate concerns, which is what keeps region logic
//! from being written once per tool:
//!
//! - **Producers**: lasso, rectangle, select-all, invert, and later a flood fill from a seed point
//!   (the magic wand — which is also what a bucket uses before it fills).
//! - **Consumers**: fill, delete, transform, and simply confining a brush.
//!
//! A bucket fill is therefore not a tool with intelligence in it. It is *find a region, fill it,
//! discard the mask* — the same region-finder the wand uses, behind a different front end. Building
//! a flood fill inside a bucket tool would guarantee a second, subtly different copy the day the
//! wand arrives, which is the mistake `export::Composite` was extracted to undo.
//!
//! # Coverage, not a bitmask
//!
//! One byte per pixel rather than one bit. It costs eight times the memory of a boolean mask, on a
//! thing that is transient and already eight times cheaper than a colour tile, and it buys
//! properties that would be expensive to retrofit onto every consumer later: anti-aliased selection
//! edges, feathering, partial selection, and soft-edged fills.
//!
//! It also makes this the same primitive a **layer mask** needs — per-pixel coverage attached to a
//! layer instead of to the document. That is a larger feature than selection for comics
//! (non-destructive erasing, masking a tone without destroying it), and it should not need a second
//! implementation.
//!
//! # The CPU holds the truth
//!
//! Deliberately, and it dissolves a problem rather than solving one. The canvas needed the whole
//! residency-and-spill machinery of `tile_store.rs` because its tiles are the only copy of the
//! artwork. A mask's authoritative copy lives here, in ordinary memory, so any GPU-side mirror is a
//! *cache*: it can be rebuilt, and running out of room for it cannot corrupt anything. It is also
//! where a flood fill wants its data, being an inherently serial algorithm.
//!
//! # Sparse by tile
//!
//! Only tiles the selection actually touches exist. A lasso around one character on a webtoon strip
//! costs the tiles it covers, not the strip.

use std::collections::HashMap;

use crate::page::PageRect;
use crate::tile::{TileCoord, TILE_SIZE, TILE_TEXELS};

/// Subsamples per axis when rasterizing a shape into coverage.
///
/// 4×4 = 16 levels of edge coverage. Not exact area sampling, which is considerably more code for a
/// difference invisible at one byte of precision — but enough that a diagonal lasso edge is smooth
/// rather than a staircase, which is the whole reason coverage is a byte.
const SUBSAMPLES: u32 = 4;

/// How much of each pixel is selected, tile by tile.
///
/// Absent tiles are entirely unselected, which is what keeps a selection cheap on a large page.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Selection {
    tiles: HashMap<TileCoord, Vec<u8>>,
}

impl Selection {
    /// A selection of nothing.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether nothing at all is selected.
    ///
    /// Checks coverage rather than merely whether tiles exist: an operation can leave a tile
    /// allocated and empty, and "there is a selection" has to mean something is in it.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        !self
            .tiles
            .values()
            .any(|t| t.iter().any(|&coverage| coverage > 0))
    }

    /// The tiles this selection touches, and their coverage.
    pub fn tiles(&self) -> impl Iterator<Item = (&TileCoord, &Vec<u8>)> {
        self.tiles.iter()
    }

    /// One tile's coverage, if this selection touches it.
    ///
    /// For a consumer that already knows which tiles it wants — a fill stages them straight into
    /// the GPU — and would otherwise pay a hash lookup per pixel to read them one at a time.
    #[must_use]
    pub fn coverage_tile(&self, coord: TileCoord) -> Option<&[u8]> {
        self.tiles.get(&coord).map(Vec::as_slice)
    }

    /// How selected a page pixel is, from 0 to 255.
    #[must_use]
    pub fn coverage_at(&self, x: i32, y: i32) -> u8 {
        let side = TILE_SIZE as i32;
        let coord = (x.div_euclid(side), y.div_euclid(side));
        let lx = x.rem_euclid(side) as usize;
        let ly = y.rem_euclid(side) as usize;
        self.tiles.get(&coord).map_or(0, |t| t[ly * TILE_SIZE + lx])
    }

    /// Write a horizontal run of coverage.
    ///
    /// For a producer that generates coverage in scanline order — the flood fill does — so a tile
    /// is resolved once per run rather than once per pixel.
    pub fn write_coverage_run(&mut self, x: i32, y: i32, values: &[u8]) {
        let mut build = Builder::default();
        std::mem::swap(&mut build.tiles, &mut self.tiles);
        build.write_run(x, y, values);
        self.tiles = build.tiles;
    }

    /// Set one pixel's coverage directly.
    ///
    /// The primitive a mask brush needs -- painting into a selection is what Quick Mask is, and a
    /// layer mask is edited the same way. Public rather than test-only because it is a real
    /// operation on a mask that happens to have arrived early, as the one convenient way to build
    /// partial coverage before feathering exists.
    pub fn set_coverage(&mut self, x: i32, y: i32, coverage: u8) {
        let mut build = Builder::default();
        std::mem::swap(&mut build.tiles, &mut self.tiles);
        build.write_run(x, y, &[coverage]);
        self.tiles = build.tiles;
    }

    /// Select the whole page.
    #[must_use]
    pub fn everything(page: PageRect) -> Self {
        let mut build = Builder::default();
        let run = vec![255_u8; page.w as usize];
        for y in page.y..page.end().1 {
            build.write_run(page.x, y, &run);
        }
        build.finish()
    }

    /// Select an axis-aligned rectangle, clipped to the page.
    ///
    /// No supersampling: the rectangle arrives already snapped to whole pixels, so every pixel is
    /// either wholly in or wholly out and there is no edge to soften.
    #[must_use]
    pub fn from_rect(rect: PageRect, page: PageRect) -> Self {
        let (ex, ey) = rect.end();
        let (page_ex, page_ey) = page.end();
        let x0 = rect.x.max(page.x);
        let x1 = ex.min(page_ex);
        let mut build = Builder::default();
        if x1 > x0 {
            let run = vec![255_u8; (x1 - x0) as usize];
            for y in rect.y.max(page.y)..ey.min(page_ey) {
                build.write_run(x0, y, &run);
            }
        }
        build.finish()
    }

    /// Select the interior of a polygon — the lasso.
    ///
    /// Implicitly closed: the last point joins the first, because a lasso the artist did not quite
    /// close is a lasso they meant to close.
    ///
    /// # Scanline, not per-pixel sampling
    ///
    /// The first version tested every pixel of the page against every edge, sixteen times over for
    /// supersampling. On a 2048² page with a two-hundred-point lasso that is over ten billion
    /// operations, and it froze the app for seconds on release — the freeze was not a slow
    /// constant, it was the wrong algorithm.
    ///
    /// This walks *sample rows* instead: for each one, find where the edges cross it, sort those
    /// crossings, and fill the spans between alternating pairs. Cost is rows × edges rather than
    /// pixels × edges × subsamples, and it only visits the polygon's own bounding box.
    ///
    /// It is also **more** accurate, not less: coverage across a span is computed as exact overlap
    /// in x, so only y is sampled. A near-horizontal edge is the one case that benefits from
    /// sampling, and it gets [`SUBSAMPLES`] rows of it.
    #[must_use]
    pub fn from_polygon(points: &[(f32, f32)], page: PageRect) -> Self {
        if points.len() < 3 {
            // A point or a line encloses nothing. An empty selection rather than something
            // degenerate keeps "did that gesture select anything" a question the caller can ask.
            return Self::new();
        }

        let (mut min_y, mut max_y) = (f32::MAX, f32::MIN);
        let (mut min_x, mut max_x) = (f32::MAX, f32::MIN);
        for &(x, y) in points {
            min_x = min_x.min(x);
            max_x = max_x.max(x);
            min_y = min_y.min(y);
            max_y = max_y.max(y);
        }
        let (page_ex, page_ey) = page.end();
        let y0 = (min_y.floor() as i32).max(page.y);
        let y1 = (max_y.ceil() as i32).min(page_ey);
        let x0 = (min_x.floor() as i32).max(page.x);
        let x1 = (max_x.ceil() as i32).min(page_ex);
        if y0 >= y1 || x0 >= x1 {
            return Self::new();
        }

        let width = (x1 - x0) as usize;
        let mut build = Builder::default();
        let mut row = vec![0.0_f32; width];
        let mut diff = vec![0.0_f32; width + 1];
        let mut out = vec![0_u8; width];
        let mut crossings: Vec<f32> = Vec::new();
        // Each sample row contributes this share of a pixel's final coverage.
        let share = 1.0 / SUBSAMPLES as f32;

        for y in y0..y1 {
            row.iter_mut().for_each(|c| *c = 0.0);
            diff.iter_mut().for_each(|c| *c = 0.0);
            for s in 0..SUBSAMPLES {
                let sy = y as f32 + (s as f32 + 0.5) * share;
                crossings.clear();
                let mut j = points.len() - 1;
                for i in 0..points.len() {
                    let (xi, yi) = points[i];
                    let (xj, yj) = points[j];
                    // Half-open in y, so a vertex sitting exactly on the sample row is counted
                    // once rather than twice or never — the classic source of single-pixel holes
                    // along a horizontal edge.
                    if (yi > sy) != (yj > sy) {
                        let t = (sy - yi) / (yj - yi);
                        crossings.push(xi + t * (xj - xi));
                    }
                    j = i;
                }
                crossings.sort_unstable_by(f32::total_cmp);
                // Alternating pairs are the inside spans: the same even-odd rule as a point test,
                // applied to a whole row at once.
                for pair in crossings.as_chunks::<2>().0 {
                    add_span(&mut row, &mut diff, x0, pair[0], pair[1], share);
                }
            }
            // One prefix sum turns the difference array into whole-pixel coverage, to which the
            // partial ends collected in `row` are added.
            let mut running = 0.0_f32;
            for (i, slot) in out.iter_mut().enumerate() {
                running += diff[i];
                *slot = ((row[i] + running) * 255.0).round().clamp(0.0, 255.0) as u8;
            }
            build.write_run(x0, y, &out);
        }
        build.finish()
    }

    /// The same mask moved by a whole-pixel offset, clipped to the page.
    ///
    /// So a selection follows the pixels it moved: a second drag then picks up the same content
    /// rather than whatever is now sitting under the old outline.
    #[must_use]
    pub fn shifted(&self, dx: i32, dy: i32, page: PageRect) -> Self {
        let side = TILE_SIZE as i32;
        let mut build = Builder::default();
        let mut run: Vec<u8> = Vec::new();
        for (coord, tile) in &self.tiles {
            for ly in 0..TILE_SIZE {
                let y = coord.1 * side + ly as i32 + dy;
                if y < page.y || y >= page.end().1 {
                    continue;
                }
                // A run at a time, so a tile is resolved once per run rather than once per pixel.
                let mut lx = 0;
                while lx < TILE_SIZE {
                    if tile[ly * TILE_SIZE + lx] == 0 {
                        lx += 1;
                        continue;
                    }
                    let start = lx;
                    run.clear();
                    while lx < TILE_SIZE && tile[ly * TILE_SIZE + lx] > 0 {
                        run.push(tile[ly * TILE_SIZE + lx]);
                        lx += 1;
                    }
                    let x = coord.0 * side + start as i32 + dx;
                    // Clipped to the page: coverage outside it could never be filled.
                    let from = x.max(page.x);
                    let to = (x + run.len() as i32).min(page.end().0);
                    if to > from {
                        let skip = (from - x) as usize;
                        build.write_run(from, y, &run[skip..skip + (to - from) as usize]);
                    }
                }
            }
        }
        build.finish()
    }

    /// Everything on the page this selection does not cover.
    ///
    /// Walks tiles rather than pixels: a per-pixel [`Selection::coverage_at`] would do a hash
    /// lookup for every pixel of the page, which is the same shape of mistake the polygon fill had.
    /// The same mask rotated, scaled and moved.
    ///
    /// The counterpart of [`Selection::shifted`] for a full transform, and it exists for one
    /// reason: after rotating a selection's *pixels*, an outline still sitting square around them
    /// describes a selection of something that is no longer there. The mask has to follow what it
    /// selected.
    ///
    /// Resampled through the same filter the pixels use, by treating coverage as alpha. Sharing the
    /// filter rather than writing a second loop is deliberate — a mask filtered differently from
    /// the pixels it covers would disagree with them at exactly the soft edges both are there to
    /// get right.
    #[must_use]
    pub fn transformed(
        &self,
        transform: &crate::Transform,
        kernel: crate::Kernel,
        page: PageRect,
    ) -> Self {
        if transform.is_a_plain_move() {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "checked to be a whole number by `is_a_plain_move`"
            )]
            return self.shifted(transform.offset.0 as i32, transform.offset.1 as i32, page);
        }
        let Some(bounds) = self.bounds() else {
            return Self::new();
        };
        let (min_x, min_y, max_x, max_y) =
            transform.bounds_of(bounds.0, bounds.1, bounds.2, bounds.3);
        let (px0, py0) = page.origin();
        let (px1, py1) = page.end();

        let mut build = Builder::default();
        crate::transform::resample(
            transform,
            kernel,
            (
                min_x.max(px0),
                min_y.max(py0),
                max_x.min(px1),
                max_y.min(py1),
            ),
            |x, y| [0.0, 0.0, 0.0, f32::from(self.coverage_at(x, y)) / 255.0],
            |x, y, texel| {
                #[expect(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "resample clamps alpha to 0..=1"
                )]
                let coverage = (texel[3] * 255.0).round() as u8;
                if coverage > 0 {
                    build.write_run(x, y, &[coverage]);
                }
            },
        );
        build.finish()
    }

    /// The page-pixel rectangle the mask's tiles cover, as `(min_x, min_y, max_x, max_y)` with the
    /// maxima exclusive.
    ///
    /// Tile-aligned rather than tight to the coverage, matching [`crate::Lifted::bounds`]: the only
    /// consumer is a resampling loop that skips empty source anyway, and a tight box would mean
    /// scanning every pixel to find it.
    #[must_use]
    pub fn bounds(&self) -> Option<(i32, i32, i32, i32)> {
        let side = TILE_SIZE as i32;
        let mut bounds: Option<(i32, i32, i32, i32)> = None;
        for coord in self.tiles.keys() {
            let (x, y) = (coord.0 * side, coord.1 * side);
            bounds = Some(match bounds {
                None => (x, y, x + side, y + side),
                Some((lx, ly, hx, hy)) => {
                    (lx.min(x), ly.min(y), hx.max(x + side), hy.max(y + side))
                }
            });
        }
        bounds
    }

    #[must_use]
    pub fn inverted(&self, page: PageRect) -> Self {
        let side = TILE_SIZE as i32;
        let (ex, ey) = page.end();
        if page.w == 0 || page.h == 0 {
            return Self::new();
        }
        let first = (page.x.div_euclid(side), page.y.div_euclid(side));
        let last = ((ex - 1).div_euclid(side), (ey - 1).div_euclid(side));

        let mut tiles = HashMap::new();
        for ty in first.1..=last.1 {
            for tx in first.0..=last.0 {
                let source = self.tiles.get(&(tx, ty));
                let mut tile = vec![0_u8; TILE_TEXELS];
                let mut any = false;
                for ly in 0..TILE_SIZE {
                    for lx in 0..TILE_SIZE {
                        let x = tx * side + lx as i32;
                        let y = ty * side + ly as i32;
                        // Clipped to the page: coverage outside it could never be filled.
                        if !page.contains(x, y) {
                            continue;
                        }
                        let i = ly * TILE_SIZE + lx;
                        let c = 255 - source.map_or(0, |t| t[i]);
                        if c > 0 {
                            tile[i] = c;
                            any = true;
                        }
                    }
                }
                if any {
                    tiles.insert((tx, ty), tile);
                }
            }
        }
        Self { tiles }
    }
}

/// A closed boundary loop, in page space. The last point joins the first.
pub type Loop = Vec<(f32, f32)>;

/// Coverage at or above this counts as inside when drawing the boundary.
///
/// Only the *outline* uses a threshold. Every operation on a selection uses the coverage itself, so
/// this decides where a line is drawn and nothing about what a fill does.
const INSIDE: u8 = 128;

impl Selection {
    /// The boundary, as closed loops ready to draw.
    ///
    /// Computed from the mask, not from the gesture that produced it. Drawing the lasso polygon we
    /// already have would look better today and be wrong tomorrow: an inversion, an intersection or
    /// a flood fill produces a mask with no polygon behind it, and then the outline and the truth
    /// would quietly disagree. One source, so they cannot.
    ///
    /// # Why loops rather than loose segments
    ///
    /// The first version returned unmerged-then-merged segments, and the marching-ants dashes were
    /// applied to each one separately. On a rectangle that is fine -- its four sides are hundreds
    /// of pixels long. On a *curve* every merged run is one to three pixels, so a four-pixel dash
    /// pattern rendered each of them as a single dot, and a lasso came out as a dotted spray.
    ///
    /// A dash pattern is a property of a path, so the boundary has to be a path. Edges are emitted
    /// with the inside on their left, which makes them chain end to end into closed loops, and the
    /// dash phase then runs continuously around each loop the way it should.
    #[must_use]
    pub fn outline(&self) -> Vec<Loop> {
        let inside = |x: i32, y: i32| self.coverage_at(x, y) >= INSIDE;

        // Directed boundary edges by start vertex. Winding consistently -- top edge to +x, right to
        // +y, bottom to -x, left to -y, with y pointing down -- is what makes them chain: the end
        // of one edge is the start of exactly one other.
        let mut next: HashMap<(i32, i32), Vec<(i32, i32)>> = HashMap::new();
        let mut edge = |from: (i32, i32), to: (i32, i32)| {
            next.entry(from).or_default().push(to);
        };
        for (coord, tile) in &self.tiles {
            let side = TILE_SIZE as i32;
            for ly in 0..TILE_SIZE {
                for lx in 0..TILE_SIZE {
                    if tile[ly * TILE_SIZE + lx] < INSIDE {
                        continue;
                    }
                    let x = coord.0 * side + lx as i32;
                    let y = coord.1 * side + ly as i32;
                    if !inside(x, y - 1) {
                        edge((x, y), (x + 1, y));
                    }
                    if !inside(x + 1, y) {
                        edge((x + 1, y), (x + 1, y + 1));
                    }
                    if !inside(x, y + 1) {
                        edge((x + 1, y + 1), (x, y + 1));
                    }
                    if !inside(x - 1, y) {
                        edge((x, y + 1), (x, y));
                    }
                }
            }
        }

        // Walk each loop, consuming edges as it goes.
        let mut starts: Vec<(i32, i32)> = next.keys().copied().collect();
        // Sorted so the output does not depend on hash order, which would make it untestable and
        // make the dash phase jump between otherwise identical frames.
        starts.sort_unstable();

        let mut loops = Vec::new();
        for start in starts {
            while next.get(&start).is_some_and(|v| !v.is_empty()) {
                let mut path = vec![start];
                let mut at = start;
                while let Some(to) = next.get_mut(&at).and_then(Vec::pop) {
                    if to == start {
                        break;
                    }
                    path.push(to);
                    at = to;
                }
                if path.len() >= 4 {
                    loops.push(simplify(&path));
                }
            }
        }
        loops
    }
}

/// Drop points that lie on a straight run, so a long edge is two points rather than two hundred.
///
/// Not cosmetic: the outline is rebuilt whenever the selection changes and redrawn every frame, and
/// a full-page selection has thousands of collinear vertices along each side.
fn simplify(path: &[(i32, i32)]) -> Loop {
    let n = path.len();
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let prev = path[(i + n - 1) % n];
        let here = path[i];
        let after = path[(i + 1) % n];
        let collinear =
            (prev.0 == here.0 && here.0 == after.0) || (prev.1 == here.1 && here.1 == after.1);
        if !collinear {
            out.push((here.0 as f32, here.1 as f32));
        }
    }
    out
}

/// Accumulates coverage into sparse tiles as it is produced.
///
/// Keeps the "only touched tiles exist" rule in one place, so every producer stays sparse without
/// each of them remembering to be.
#[derive(Default)]
struct Builder {
    tiles: HashMap<TileCoord, Vec<u8>>,
}

impl Builder {
    /// Write a horizontal run of coverage starting at `(x, y)`.
    ///
    /// Resolves a tile once per run rather than once per pixel. Writing pixel by pixel means a hash
    /// lookup each time -- for select-all on a 2048-square page that is four million lookups, which
    /// is the same shape of mistake the polygon fill originally had, just quieter.
    ///
    /// Zero coverage is skipped rather than stored, which is what keeps a selection sparse.
    fn write_run(&mut self, x: i32, y: i32, values: &[u8]) {
        let side = TILE_SIZE as i32;
        let ty = y.div_euclid(side);
        let ly = y.rem_euclid(side) as usize;

        let mut written = 0;
        while written < values.len() {
            let x_here = x + written as i32;
            let tx = x_here.div_euclid(side);
            let lx = x_here.rem_euclid(side) as usize;
            // How much of this run lands in this tile.
            let take = (TILE_SIZE - lx).min(values.len() - written);
            let chunk = &values[written..written + take];

            if chunk.iter().any(|&c| c > 0) {
                let tile = self
                    .tiles
                    .entry((tx, ty))
                    .or_insert_with(|| vec![0_u8; TILE_TEXELS]);
                let base = ly * TILE_SIZE + lx;
                for (slot, &c) in tile[base..base + take].iter_mut().zip(chunk) {
                    if c > 0 {
                        *slot = c;
                    }
                }
            }
            written += take;
        }
    }

    fn finish(self) -> Selection {
        Selection { tiles: self.tiles }
    }
}

/// Add a horizontal span's coverage to a row.
///
/// The two partially covered pixels at the ends go straight into `row`; everything between them is
/// a whole pixel and goes into `diff`, a difference array summed once per pixel row afterwards.
/// That makes a span cost O(1) rather than O(its width) -- which matters because there are
/// [`SUBSAMPLES`] sample rows per pixel row, and paying the width on every one of them was most of
/// the time that remained after the scanline rewrite.
///
/// Exact overlap at the ends rather than sampling: a pixel the span covers halfway gets exactly
/// half. Getting the ends right is what lets y be the only axis that needs supersampling at all.
fn add_span(row: &mut [f32], diff: &mut [f32], x_origin: i32, from: f32, to: f32, weight: f32) {
    let width = row.len() as f32;
    let a = (from - x_origin as f32).clamp(0.0, width);
    let b = (to - x_origin as f32).clamp(0.0, width);
    if b <= a {
        return;
    }
    let first = a.floor() as usize;
    let last = b.floor() as usize;

    if first == last {
        // Entirely inside one pixel.
        if first < row.len() {
            row[first] += (b - a) * weight;
        }
        return;
    }
    if first < row.len() {
        row[first] += ((first + 1) as f32 - a) * weight;
    }
    if last < row.len() {
        row[last] += (b - last as f32) * weight;
    }
    // The whole pixels strictly between the two ends.
    if last > first + 1 {
        diff[first + 1] += weight;
        diff[last] -= weight;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page() -> PageRect {
        PageRect::from_size(400, 400)
    }

    /// A realistic lasso on a realistic page must not take perceptible time.
    ///
    /// This is a regression test for a shipped freeze, not a benchmark. The first implementation
    /// tested every pixel of the page against every edge, sixteen times over — on this input that
    /// is roughly ten billion operations, and the app locked up for seconds when the pen lifted.
    ///
    /// The bound is deliberately loose. It is not trying to measure anything; it is trying to be
    /// impossible to pass with the wrong *algorithm*, while being impossible to fail on a slow
    /// machine with the right one. Scanline does this in single-digit milliseconds.
    #[test]
    fn a_lasso_on_a_full_page_is_not_slow() {
        let page = PageRect::from_size(2048, 2048);
        // A 300-point circle, the shape a freehand lasso actually produces.
        let points: Vec<(f32, f32)> = (0..300)
            .map(|i| {
                let a = i as f32 * std::f32::consts::TAU / 300.0;
                (1024.0 + 900.0 * a.cos(), 1024.0 + 900.0 * a.sin())
            })
            .collect();

        let started = std::time::Instant::now();
        let sel = Selection::from_polygon(&points, page);
        let took = started.elapsed();

        assert!(!sel.is_empty(), "the lasso selected nothing");
        assert_eq!(sel.coverage_at(1024, 1024), 255, "the centre is inside");
        assert_eq!(sel.coverage_at(20, 20), 0, "the corner is outside");
        assert!(
            took < std::time::Duration::from_millis(500),
            "a 300-point lasso on a 2048 page took {took:?}; that is an algorithm problem,              not a slow machine"
        );
    }

    #[test]
    fn an_empty_selection_has_no_outline() {
        assert!(Selection::new().outline().is_empty());
    }

    /// A rectangle's boundary is one loop of four corners, not four hundred vertices.
    ///
    /// Both halves matter. **One loop**, because dashes are a property of a path: applying them to
    /// loose segments is what turned a curved lasso into a spray of dots. **Four corners**, because
    /// the outline is redrawn every frame and a full-page selection has thousands of collinear
    /// vertices along each side.
    #[test]
    fn a_rectangles_outline_is_one_loop_of_four_corners() {
        let sel = Selection::from_rect(PageRect::new(100, 100, 50, 40), page());
        let outline = sel.outline();
        assert_eq!(outline.len(), 1, "expected one loop, got {outline:?}");
        assert_eq!(
            outline[0].len(),
            4,
            "expected four corners, got {:?}",
            outline[0]
        );

        let path = &outline[0];
        let perimeter: f32 = (0..path.len())
            .map(|i| {
                let a = path[i];
                let b = path[(i + 1) % path.len()];
                (b.0 - a.0).abs() + (b.1 - a.1).abs()
            })
            .sum();
        assert!(
            (perimeter - 2.0 * (50.0 + 40.0)).abs() < 0.01,
            "the perimeter came to {perimeter}, not 2*(50+40)"
        );
    }

    /// A curved boundary comes out as one continuous loop, not a heap of fragments.
    ///
    /// This is the reported bug: the outline traced the right shape but rendered as tiny dots,
    /// because every fragment was dashed on its own and a curve's fragments are one to three pixels
    /// long. Fragment count is the thing to assert -- a single closed path dashes correctly, and
    /// two hundred of them cannot.
    #[test]
    fn a_curved_outline_is_one_continuous_loop() {
        let points: Vec<(f32, f32)> = (0..64)
            .map(|i| {
                let a = i as f32 * std::f32::consts::TAU / 64.0;
                (200.0 + 80.0 * a.cos(), 200.0 + 80.0 * a.sin())
            })
            .collect();
        let outline = Selection::from_polygon(&points, page()).outline();

        assert_eq!(
            outline.len(),
            1,
            "a circle's boundary came out as {} pieces; dashes would render it as dots",
            outline.len()
        );
        // A staircase around a circle of radius 80 has plenty of corners, but nothing like one per
        // boundary pixel -- the collinear runs between them are what `simplify` removes.
        assert!(
            outline[0].len() > 20,
            "the loop has only {} corners; it is not tracing the curve",
            outline[0].len()
        );
    }

    /// The outline traces the mask, so a hole in the selection is drawn.
    ///
    /// This is why it is computed from coverage rather than from the lasso polygon: after an
    /// inversion there is no polygon, and an outline that only knew about gestures would show the
    /// wrong shape the first time anyone inverted anything.
    #[test]
    fn an_inverted_selection_outlines_the_hole_and_the_page_edge() {
        let sel = Selection::from_rect(PageRect::new(100, 100, 50, 40), page()).inverted(page());
        let outline = sel.outline();
        // Two loops: the page border, and the hole where the rectangle was.
        assert_eq!(
            outline.len(),
            2,
            "expected the page border and the hole, got {outline:?}"
        );
        assert!(
            outline.iter().all(|l| l.len() == 4),
            "both loops are rectangles, so both should have four corners: {outline:?}"
        );
    }

    /// The mask follows the pixels a move took, so a second drag picks up the same content.
    #[test]
    fn shifting_moves_the_mask_and_clips_to_the_page() {
        let sel = Selection::from_rect(PageRect::new(100, 100, 40, 40), page());
        let moved = sel.shifted(30, -20, page());

        assert_eq!(moved.coverage_at(130, 80), 255, "the moved corner");
        assert_eq!(moved.coverage_at(169, 119), 255, "and its far corner");
        assert_eq!(moved.coverage_at(100, 100), 0, "nothing left behind");
        assert_eq!(moved.coverage_at(170, 80), 0, "nor past the new edge");
    }

    /// Partial coverage has to survive the move, or a feathered selection would harden the first
    /// time it was dragged.
    #[test]
    fn shifting_preserves_partial_coverage() {
        let mut sel = Selection::from_rect(PageRect::new(100, 100, 40, 40), page());
        sel.set_coverage(120, 120, 77);
        let moved = sel.shifted(10, 10, page());
        assert_eq!(moved.coverage_at(130, 130), 77);
        assert_eq!(moved.coverage_at(131, 130), 255);
    }

    /// Moved past the edge, the part that leaves the page is dropped rather than wrapping.
    #[test]
    fn shifting_off_the_page_clips_rather_than_wrapping() {
        let sel = Selection::from_rect(PageRect::new(10, 10, 40, 40), page());
        let moved = sel.shifted(-30, -30, page());
        assert_eq!(moved.coverage_at(0, 0), 255, "the part still on the page");
        assert_eq!(moved.coverage_at(19, 19), 255, "up to its new far corner");
        assert_eq!(
            moved.coverage_at(390, 390),
            0,
            "coverage wrapped around to the opposite corner"
        );
    }

    #[test]
    fn nothing_is_selected_to_begin_with() {
        assert!(Selection::new().is_empty());
        assert_eq!(Selection::new().coverage_at(10, 10), 0);
    }

    #[test]
    fn a_rectangle_selects_exactly_itself() {
        let sel = Selection::from_rect(PageRect::new(100, 100, 50, 50), page());
        assert_eq!(sel.coverage_at(100, 100), 255, "the first pixel is inside");
        assert_eq!(sel.coverage_at(149, 149), 255, "and so is the last");
        assert_eq!(sel.coverage_at(99, 100), 0, "one pixel left of it is not");
        assert_eq!(sel.coverage_at(150, 100), 0, "nor one pixel past the end");
        assert!(!sel.is_empty());
    }

    /// The reason coverage is a byte rather than a bit.
    ///
    /// A diagonal edge must come out smooth, not as a staircase. Asserts the *existence* of partial
    /// coverage along the edge rather than exact values, which would only pin the subsample count.
    #[test]
    fn a_diagonal_edge_is_anti_aliased() {
        // A triangle with one 45° edge.
        let sel = Selection::from_polygon(&[(50.0, 50.0), (250.0, 50.0), (50.0, 250.0)], page());

        // Counted over the whole triangle rather than along the exact line: a pixel the edge
        // passes through the centre of has all its subsamples on one side, so the partially
        // covered pixels sit just inside it. Scanning avoids the test asserting where they land.
        let mut partial = 0;
        for y in 40..260 {
            for x in 40..260 {
                let c = sel.coverage_at(x, y);
                if c > 0 && c < 255 {
                    partial += 1;
                }
            }
        }
        assert!(
            partial > 100,
            "a 45° edge produced only {partial} partially covered pixels; it is a staircase"
        );

        // And the interior and exterior are still unambiguous.
        assert_eq!(sel.coverage_at(60, 60), 255, "well inside");
        assert_eq!(sel.coverage_at(240, 240), 0, "well outside");
    }

    #[test]
    fn a_lasso_needs_three_points_to_enclose_anything() {
        assert!(Selection::from_polygon(&[], page()).is_empty());
        assert!(Selection::from_polygon(&[(10.0, 10.0)], page()).is_empty());
        assert!(Selection::from_polygon(&[(10.0, 10.0), (99.0, 99.0)], page()).is_empty());
    }

    /// A concave shape is the case an even-odd test has to get right, and the one a naive
    /// "inside if any edge lies to the right" implementation quietly gets wrong.
    ///
    /// The notch opens **left**, deliberately. A point inside a right-opening notch has *no* edges
    /// to its right, so it reads as outside under either rule and proves nothing — the first
    /// version of this test was exactly that, and a sabotage replacing the even-odd toggle with an
    /// unconditional `inside = true` sailed through it. Here the notch point has two edges to its
    /// right: even, therefore outside, and only a real parity test says so.
    #[test]
    fn a_concave_lasso_excludes_its_notch() {
        // A "C" reversed: spine down the right at x = 240..300, arms reaching left to x = 100.
        let sel = Selection::from_polygon(
            &[
                (100.0, 100.0),
                (300.0, 100.0),
                (300.0, 300.0),
                (100.0, 300.0),
                (100.0, 260.0),
                (240.0, 260.0),
                (240.0, 140.0),
                (100.0, 140.0),
            ],
            page(),
        );

        assert_eq!(sel.coverage_at(270, 200), 255, "the spine");
        assert_eq!(sel.coverage_at(150, 120), 255, "the top arm");
        assert_eq!(sel.coverage_at(150, 280), 255, "the bottom arm");
        assert_eq!(
            sel.coverage_at(150, 200),
            0,
            "the notch was selected: two crossings to the right is even, therefore outside"
        );
    }

    #[test]
    fn a_selection_is_clipped_to_the_page() {
        // Deliberately hangs off the top-left and bottom-right.
        let sel = Selection::from_rect(PageRect::new(-50, -50, 500, 500), page());
        assert_eq!(sel.coverage_at(0, 0), 255);
        assert_eq!(sel.coverage_at(399, 399), 255);
        assert_eq!(
            sel.coverage_at(-1, -1),
            0,
            "coverage outside the page could never be filled"
        );
        assert_eq!(sel.coverage_at(400, 400), 0);
    }

    /// Sparseness is what makes a selection affordable on a webtoon strip.
    #[test]
    fn only_touched_tiles_are_stored() {
        // A page four tiles wide and four tall; the selection sits inside one of them.
        let sel = Selection::from_rect(
            PageRect::new(10, 10, 40, 40),
            PageRect::from_size(1024, 1024),
        );
        assert_eq!(
            sel.tiles().count(),
            1,
            "a small selection allocated tiles it does not cover"
        );
    }

    #[test]
    fn everything_selects_the_whole_page_and_nothing_beyond() {
        let sel = Selection::everything(page());
        assert_eq!(sel.coverage_at(0, 0), 255);
        assert_eq!(sel.coverage_at(399, 399), 255);
        assert_eq!(sel.coverage_at(400, 0), 0);
        assert_eq!(sel.tiles().count(), 4, "400px is two tiles each way");
    }

    #[test]
    fn inverting_swaps_what_is_selected() {
        let sel = Selection::from_rect(PageRect::new(100, 100, 50, 50), page());
        let flipped = sel.inverted(page());

        assert_eq!(flipped.coverage_at(120, 120), 0, "inside became outside");
        assert_eq!(flipped.coverage_at(10, 10), 255, "outside became inside");
        assert_eq!(
            flipped.coverage_at(400, 400),
            0,
            "inverting must not select beyond the page"
        );

        // Twice round is where an off-by-one in the complement shows up.
        let back = flipped.inverted(page());
        assert_eq!(back.coverage_at(120, 120), 255);
        assert_eq!(back.coverage_at(10, 10), 0);
    }

    /// Partial coverage has to survive inversion proportionally, or feathered edges would harden
    /// the first time anyone inverted a selection.
    #[test]
    fn inverting_preserves_partial_coverage() {
        let sel = Selection::from_polygon(&[(50.0, 50.0), (250.0, 50.0), (50.0, 250.0)], page());
        let flipped = sel.inverted(page());
        let mut checked = 0;
        for y in 40..260 {
            for x in 40..260 {
                let a = sel.coverage_at(x, y);
                if a > 0 && a < 255 {
                    assert_eq!(
                        flipped.coverage_at(x, y),
                        255 - a,
                        "partial coverage at ({x}, {y}) did not invert proportionally"
                    );
                    checked += 1;
                }
            }
        }
        assert!(
            checked > 100,
            "found only {checked} partial pixels to check"
        );
    }
    /// A whole-pixel move must take the exact path, not the filter: a mask softened on every drag
    /// would creep outward one anti-aliased pixel at a time.
    #[test]
    fn a_whole_pixel_move_of_a_mask_is_exact() {
        let sel = Selection::from_rect(PageRect::new(100, 100, 60, 40), page());
        let shifted = sel.shifted(23, -11, page());
        let transformed = sel.transformed(
            &crate::Transform::translation(23.0, -11.0),
            crate::Kernel::Mitchell,
            page(),
        );
        assert_eq!(transformed, shifted);
    }

    /// The point of the whole thing: after rotating, the mask covers what the pixels now cover.
    #[test]
    fn a_rotated_mask_follows_what_it_selected() {
        // A tall strip, so a turn is visible rather than a symmetry.
        let sel = Selection::from_rect(PageRect::new(96, 60, 16, 120), page());
        let t = crate::Transform {
            pivot: (104.0, 120.0),
            rotation: std::f32::consts::FRAC_PI_2,
            ..crate::Transform::IDENTITY
        };
        let out = sel.transformed(&t, crate::Kernel::Mitchell, page());

        assert!(
            out.coverage_at(150, 120) > 200,
            "the turned strip should reach well right of the pivot"
        );
        assert!(out.coverage_at(60, 120) > 200, "and well left of it");
        assert!(
            out.coverage_at(104, 170) < 40,
            "and no longer reach far below it"
        );
    }

    /// Scaling a mask up selects more, and down selects less. Catches an inverse applied the wrong
    /// way round, which otherwise looks plausible.
    #[test]
    fn scaling_a_mask_changes_how_much_is_selected() {
        let sel = Selection::from_rect(PageRect::new(200, 200, 80, 80), page());
        let selected = |t: &crate::Transform| {
            sel.transformed(t, crate::Kernel::Mitchell, page())
                .tiles()
                .map(|(_, cov)| cov.iter().filter(|&&c| c > 128).count())
                .sum::<usize>()
        };
        let plain = selected(&crate::Transform {
            pivot: (240.0, 240.0),
            ..crate::Transform::IDENTITY
        });
        let bigger = selected(&crate::Transform {
            pivot: (240.0, 240.0),
            scale: (2.0, 2.0),
            ..crate::Transform::IDENTITY
        });
        assert!(
            bigger > plain * 3,
            "doubling should roughly quadruple the area: {plain} -> {bigger}"
        );
    }

    /// A transformed mask stays on the page, like every other producer.
    #[test]
    fn a_transformed_mask_is_clipped_to_the_page() {
        let sel = Selection::from_rect(PageRect::new(0, 0, 100, 100), page());
        let t = crate::Transform {
            offset: (-400.0, -400.0),
            rotation: 0.3,
            ..crate::Transform::IDENTITY
        };
        let out = sel.transformed(&t, crate::Kernel::Mitchell, page());
        assert!(
            out.is_empty(),
            "a mask moved entirely off the page should select nothing"
        );
    }

    #[test]
    fn transforming_an_empty_mask_gives_an_empty_mask() {
        let out = Selection::new().transformed(
            &crate::Transform {
                rotation: 0.5,
                ..crate::Transform::IDENTITY
            },
            crate::Kernel::Mitchell,
            page(),
        );
        assert!(out.is_empty());
        assert!(Selection::new().bounds().is_none());
    }
}
