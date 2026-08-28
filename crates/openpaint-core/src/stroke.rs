//! Per-stroke accumulation — what makes *flow* and *opacity* mean what they mean.
//!
//! # The model (Photoshop / CSP)
//!
//! - **Flow** is how much paint each individual dab deposits.
//! - **Opacity** is the ceiling the whole stroke may reach, no matter how many
//!   dabs land on the same pixel.
//!
//! So with flow 10% and opacity 50%, dragging back and forth over one spot
//! *builds up* toward 50% and then stops. Lifting the pen and stroking again
//! builds from 50% toward higher — because the ceiling is **per stroke**, not per
//! layer. That distinction is the whole point of this module, and it is the thing
//! naive implementations get wrong.
//!
//! Compositing each dab straight onto the canvas cannot express it: with flow
//! 10%, twenty overlapping dabs would reach ~88% instead of stopping at 50%, so
//! slow strokes go blotchy exactly where the pen lingers.
//!
//! # How it works
//!
//! Two pieces of per-stroke state:
//!
//! 1. **An accumulation buffer** holding, per pixel, how much paint this stroke
//!    has deposited so far (before the opacity ceiling). Each dab accumulates
//!    `a += flow * coverage * (1 - a)`, which approaches 1.0 asymptotically and
//!    can never exceed it — the same "over" arithmetic as compositing, applied to
//!    a scalar.
//! 2. **A snapshot** of the canvas tiles the stroke touches, taken before the
//!    stroke modified them.
//!
//! The snapshot is what allows the visible result to be *recomputed* rather than
//! progressively darkened: every update re-composites affected tiles as
//! `snapshot → (color × a × opacity)`. Recomputation is idempotent, which is why
//! live feedback mid-stroke and the final result agree exactly.
//!
//! # This snapshot is also undo's foundation
//!
//! Keeping the pre-stroke state of touched tiles is precisely the copy-on-write
//! tile snapshot that undo needs (OPEN_QUESTIONS Q13). Two of the largest Phase-1
//! items — a correct brush and undo — turn out to share one mechanism, which is
//! why this lands before either.
//!
//! # Reference implementation
//!
//! Like [`crate::raster`], this is the CPU reference (DECISIONS §4a). Clarity
//! over speed. Only tiles touched since the last composite are recomputed, so
//! cost tracks new dabs rather than total stroke length — without that, a long
//! stroke would be quadratic.

use std::collections::{HashMap, HashSet};

use crate::canvas::Canvas;
use crate::color::{over_premul, scale_premul};
use crate::dab::Dab;
use crate::tile::{Tile, TileCoord, TILE_SIZE, TILE_TEXELS};

/// Accumulated coverage for one tile, one scalar per pixel.
///
/// `f32` rather than `f16` because this is transient working state, not stored
/// pixels: many dabs accumulate into it, and rounding at every step would drift.
type AccumTile = Vec<f32>;

/// Per-stroke paint accumulation and the pre-stroke snapshot it composites over.
///
/// Create one and reuse it across strokes; call [`StrokePainter::begin`] at the
/// start of each.
#[derive(Default)]
pub struct StrokePainter {
    /// How much paint this stroke has deposited, per pixel, pre-opacity.
    accum: HashMap<TileCoord, AccumTile>,
    /// Canvas tiles as they were before this stroke touched them.
    snapshot: HashMap<TileCoord, Tile>,
    /// Tiles whose accumulation changed since the last composite.
    pending: HashSet<TileCoord>,
}

impl StrokePainter {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Start a new stroke, discarding any previous stroke's state.
    ///
    /// Retains allocations — strokes happen constantly, and this is on the
    /// interactive path.
    pub fn begin(&mut self) {
        self.accum.clear();
        self.snapshot.clear();
        self.pending.clear();
    }

    /// Whether this stroke has deposited anything yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.accum.is_empty()
    }

    /// Tiles this stroke has touched. This is the set undo would need to restore.
    pub fn touched_tiles(&self) -> impl Iterator<Item = &TileCoord> {
        self.snapshot.keys()
    }

    /// Accumulate one dab's paint, snapshotting any tile it touches for the first
    /// time.
    pub fn add_dab(&mut self, canvas: &Canvas, dab: &Dab) {
        if dab.radius <= 0.0 || dab.flow <= 0.0 {
            return;
        }
        let (min_x, min_y, max_x, max_y) = dab.pixel_bounds();
        let flow = dab.flow.clamp(0.0, 1.0);

        for y in min_y..=max_y {
            for x in min_x..=max_x {
                if x < 0 || y < 0 || x >= canvas.width() as i32 || y >= canvas.height() as i32 {
                    continue;
                }
                let coverage = dab.coverage_at(x as f32 + 0.5, y as f32 + 0.5);
                if coverage <= 0.0 {
                    continue;
                }

                let coord = (
                    x.div_euclid(TILE_SIZE as i32),
                    y.div_euclid(TILE_SIZE as i32),
                );
                let lx = x.rem_euclid(TILE_SIZE as i32) as usize;
                let ly = y.rem_euclid(TILE_SIZE as i32) as usize;

                // Snapshot before this stroke changes anything in the tile.
                self.snapshot.entry(coord).or_insert_with(|| {
                    canvas
                        .tile(coord)
                        .cloned()
                        .unwrap_or_else(|| Tile::filled(Canvas::paper_color()))
                });

                let accum = self
                    .accum
                    .entry(coord)
                    .or_insert_with(|| vec![0.0; TILE_TEXELS]);

                // Accumulate toward 1.0 without ever exceeding it.
                let a = &mut accum[ly * TILE_SIZE + lx];
                *a += flow * coverage * (1.0 - *a);

                self.pending.insert(coord);
            }
        }
    }

    /// Accumulate a batch of dabs, in order.
    pub fn add_dabs(&mut self, canvas: &Canvas, dabs: &[Dab]) {
        for dab in dabs {
            self.add_dab(canvas, dab);
        }
    }

    /// Re-composite tiles changed since the last call: `snapshot → paint`.
    ///
    /// Idempotent per tile, which is what keeps mid-stroke previews and the final
    /// result identical. `color_linear_premul` is the stroke color (normally
    /// opaque) and `opacity` is the stroke's ceiling.
    pub fn composite(&mut self, canvas: &mut Canvas, color_linear_premul: [f32; 4], opacity: f32) {
        let opacity = opacity.clamp(0.0, 1.0);
        for coord in self.pending.drain() {
            let Some(accum) = self.accum.get(&coord) else {
                continue;
            };
            let Some(snapshot) = self.snapshot.get(&coord) else {
                continue;
            };

            for ly in 0..TILE_SIZE {
                for lx in 0..TILE_SIZE {
                    let a = accum[ly * TILE_SIZE + lx];
                    if a <= 0.0 {
                        continue;
                    }
                    let src = scale_premul(color_linear_premul, a * opacity);
                    let dst = snapshot.texel(lx, ly);
                    let x = coord.0 * TILE_SIZE as i32 + lx as i32;
                    let y = coord.1 * TILE_SIZE as i32 + ly as i32;
                    canvas.replace_pixel(x, y, over_premul(src, dst));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLACK: [f32; 4] = [0.0, 0.0, 0.0, 1.0];

    fn dab_at(x: f32, y: f32, flow: f32) -> Dab {
        Dab {
            x,
            y,
            radius: 6.0,
            // Hard-edged, so the center pixel gets coverage exactly 1.0 and the
            // arithmetic under test isn't entangled with the falloff curve.
            hardness: 0.0,
            flow,
            color_linear_premul: BLACK,
        }
    }

    /// How much paint ended up at `(x, y)`, as an effective alpha in `0.0..=1.0`.
    ///
    /// Derived from the *color* channel, not alpha: the canvas is opaque paper, so
    /// its alpha is always 1.0 regardless of how much paint landed. Compositing
    /// black at effective alpha `k` over paper `p` leaves `p * (1 - k)`, so
    /// `k = 1 - color / p`.
    fn paint_amount(canvas: &Canvas, x: usize, y: usize) -> f32 {
        let paper = Canvas::paper_color()[0];
        match canvas.tile((0, 0)) {
            Some(tile) => 1.0 - tile.texel(x, y)[0] / paper,
            None => 0.0,
        }
    }

    /// Paint one spot repeatedly and report how much paint landed there.
    fn build_up(dab_count: usize, flow: f32, opacity: f32) -> f32 {
        let mut canvas = Canvas::new(256, 256);
        let mut painter = StrokePainter::new();
        painter.begin();
        for _ in 0..dab_count {
            painter.add_dab(&canvas, &dab_at(100.5, 100.5, flow));
        }
        painter.composite(&mut canvas, BLACK, opacity);
        paint_amount(&canvas, 100, 100)
    }

    #[test]
    fn a_single_full_dab_is_fully_opaque() {
        assert!((build_up(1, 1.0, 1.0) - 1.0).abs() < 1e-3);
    }

    #[test]
    fn flow_controls_how_much_a_single_dab_deposits() {
        assert!((build_up(1, 0.25, 1.0) - 0.25).abs() < 1e-3);
    }

    /// The defining behavior: overlapping dabs build up but never pass opacity.
    #[test]
    fn overlapping_dabs_build_up_toward_opacity_and_stop() {
        let ceiling = 0.5;
        let few = build_up(3, 0.1, ceiling);
        let many = build_up(200, 0.1, ceiling);
        assert!(few < ceiling, "3 dabs already at ceiling: {few}");
        assert!(many <= ceiling + 1e-3, "exceeded ceiling: {many}");
        assert!(
            (many - ceiling).abs() < 1e-2,
            "did not reach ceiling: {many}"
        );
        assert!(many > few, "did not build up at all");
    }

    /// Naive per-dab compositing would sail past the ceiling. This is the
    /// regression guard for the bug this module exists to fix.
    #[test]
    fn opacity_is_a_hard_ceiling_not_a_multiplier_per_dab() {
        // 20 dabs at flow 0.1 reach ~0.88 of the way, which without a per-stroke
        // ceiling would render at ~0.88 rather than the requested 0.3.
        let a = build_up(20, 0.1, 0.3);
        assert!(a <= 0.3 + 1e-3, "got {a}, must not exceed 0.3");
    }

    #[test]
    fn build_up_is_monotonic_in_dab_count() {
        let mut prev = 0.0;
        for n in [1, 2, 4, 8, 16] {
            let a = build_up(n, 0.2, 1.0);
            assert!(a >= prev - 1e-6, "{n} dabs went backwards: {a} < {prev}");
            prev = a;
        }
    }

    /// The ceiling is per *stroke*. A second stroke must be able to go darker,
    /// or repeated passes could never deepen a tone.
    #[test]
    fn a_second_stroke_builds_on_top_of_the_first() {
        let mut canvas = Canvas::new(256, 256);
        let mut painter = StrokePainter::new();

        painter.begin();
        for _ in 0..50 {
            painter.add_dab(&canvas, &dab_at(100.5, 100.5, 0.1));
        }
        painter.composite(&mut canvas, BLACK, 0.5);
        let after_first = paint_amount(&canvas, 100, 100);

        painter.begin();
        for _ in 0..50 {
            painter.add_dab(&canvas, &dab_at(100.5, 100.5, 0.1));
        }
        painter.composite(&mut canvas, BLACK, 0.5);
        let after_second = paint_amount(&canvas, 100, 100);

        assert!(
            after_second > after_first + 0.1,
            "second stroke did not deepen: {after_first} -> {after_second}"
        );
    }

    /// Compositing partway through must give the same answer as compositing once
    /// at the end, or previews would disagree with the committed result.
    #[test]
    fn incremental_compositing_matches_a_single_final_composite() {
        let dabs: Vec<Dab> = (0..10)
            .map(|i| dab_at(100.5 + i as f32, 100.5, 0.3))
            .collect();

        let mut incremental = Canvas::new(256, 256);
        let mut p = StrokePainter::new();
        p.begin();
        for d in &dabs {
            p.add_dab(&incremental, d);
            p.composite(&mut incremental, BLACK, 0.7);
        }

        let mut at_end = Canvas::new(256, 256);
        let mut q = StrokePainter::new();
        q.begin();
        q.add_dabs(&at_end, &dabs);
        q.composite(&mut at_end, BLACK, 0.7);

        let a = incremental.tile((0, 0)).expect("tile");
        let b = at_end.tile((0, 0)).expect("tile");
        for y in 90..112 {
            for x in 90..122 {
                assert_eq!(a.texel(x, y), b.texel(x, y), "differ at ({x},{y})");
            }
        }
    }

    #[test]
    fn zero_flow_deposits_nothing() {
        let mut canvas = Canvas::new(256, 256);
        let mut painter = StrokePainter::new();
        painter.begin();
        painter.add_dab(&canvas, &dab_at(100.5, 100.5, 0.0));
        painter.composite(&mut canvas, BLACK, 1.0);
        assert!(painter.is_empty());
        assert_eq!(canvas.tiles().count(), 0);
    }

    #[test]
    fn zero_opacity_leaves_the_canvas_untouched() {
        assert!(build_up(10, 1.0, 0.0) < 1e-3);
    }

    #[test]
    fn begin_discards_the_previous_stroke() {
        let canvas = Canvas::new(256, 256);
        let mut painter = StrokePainter::new();
        painter.begin();
        painter.add_dab(&canvas, &dab_at(100.5, 100.5, 1.0));
        assert!(!painter.is_empty());
        painter.begin();
        assert!(painter.is_empty());
        assert_eq!(painter.touched_tiles().count(), 0);
    }

    #[test]
    fn a_stroke_across_a_tile_seam_snapshots_both_tiles() {
        let canvas = Canvas::new(1024, 1024);
        let mut painter = StrokePainter::new();
        painter.begin();
        painter.add_dab(&canvas, &dab_at(TILE_SIZE as f32, 100.0, 1.0));
        assert_eq!(painter.touched_tiles().count(), 2);
    }
}
