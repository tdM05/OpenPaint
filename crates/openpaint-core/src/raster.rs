//! CPU dab rasterization — the **reference implementation**.
//!
//! This is the other side of the boundary described in [`crate::dab`]: it turns
//! [`Dab`]s into pixels. Per DECISIONS §4a the fast path for this moves to the
//! GPU, and this module stays as the reference that GPU output is tested
//! against. That is what makes chasing Photoshop's falloff curve (docs Q7a)
//! verifiable rather than a matter of opinion.
//!
//! Consequently: **optimize for clarity, not speed.** If this and the GPU
//! disagree, this one is right by definition, so it has to be obviously correct.
//!
//! ⚠️ Status check: there is currently **no production call site** — strokes paint
//! through [`crate::stroke::StrokePainter`], which is the correct path for
//! anything with more than one dab. So the "reference implementation" role is real
//! but not yet earned; it becomes load-bearing when the GPU rasterizer lands and
//! has something to be compared against. Tracked in OPEN_QUESTIONS Q15.

use crate::canvas::Canvas;
use crate::color::scale_premul;
use crate::dab::Dab;

/// Rasterize one dab onto a canvas, compositing it directly.
///
/// ⚠️ Direct compositing is **not** how strokes are painted — use
/// [`crate::stroke::StrokePainter`] for that, or overlapping dabs will darken on
/// every overlap instead of building toward the stroke's opacity ceiling. This
/// remains useful for single isolated dabs and as the simplest expression of the
/// coverage/blend math for tests to check against.
pub fn rasterize_dab(canvas: &mut Canvas, dab: &Dab, falloff: &crate::Curve) {
    if dab.radius <= 0.0 || dab.color_linear_premul[3] <= 0.0 {
        return;
    }

    let (min_x, min_y, max_x, max_y) = dab.pixel_bounds();

    for y in min_y..=max_y {
        for x in min_x..=max_x {
            // Sample at the pixel center.
            let coverage = dab.coverage_at(x as f32 + 0.5, y as f32 + 0.5, falloff);
            if coverage > 0.0 {
                // Premultiplied, so coverage scales all four channels.
                let amount = coverage * dab.flow.clamp(0.0, 1.0);
                canvas.blend_pixel(x, y, scale_premul(dab.color_linear_premul, amount));
            }
        }
    }
}

/// Rasterize a whole batch of dabs in order.
///
/// Order matters and must be preserved: dabs composite over one another, so
/// this cannot be reordered or parallelized naively.
pub fn rasterize_dabs(canvas: &mut Canvas, dabs: &[Dab], falloff: &crate::Curve) {
    for dab in dabs {
        rasterize_dab(canvas, dab, falloff);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dab::linear_falloff;
    use crate::tile::TILE_SIZE;

    fn black_dab(x: f32, y: f32, radius: f32, hardness: f32) -> Dab {
        Dab {
            x,
            y,
            radius,
            hardness,
            flow: 1.0,
            roundness: 1.0,
            angle: 0.0,
            color_linear_premul: [0.0, 0.0, 0.0, 1.0],
        }
    }

    #[test]
    fn a_dab_paints_something() {
        let mut c = Canvas::new(256, 256);
        rasterize_dab(
            &mut c,
            &black_dab(128.0, 128.0, 8.0, 0.5),
            &linear_falloff(),
        );
        assert_eq!(c.tiles().count(), 1);
    }

    #[test]
    fn a_zero_radius_dab_paints_nothing() {
        let mut c = Canvas::new(256, 256);
        rasterize_dab(
            &mut c,
            &black_dab(128.0, 128.0, 0.0, 0.5),
            &linear_falloff(),
        );
        assert_eq!(c.tiles().count(), 0);
    }

    #[test]
    fn a_fully_transparent_dab_paints_nothing() {
        let mut c = Canvas::new(256, 256);
        let mut d = black_dab(128.0, 128.0, 8.0, 0.5);
        d.color_linear_premul = [0.0; 4];
        rasterize_dab(&mut c, &d, &linear_falloff());
        assert_eq!(c.tiles().count(), 0);
    }

    /// A hard dab's centre must land at the dab colour exactly, at full coverage. Catches
    /// coverage and premultiplication mistakes.
    #[test]
    fn a_hard_dab_center_is_fully_opaque_paint() {
        let mut c = Canvas::new(256, 256);
        rasterize_dab(
            &mut c,
            &black_dab(100.5, 100.5, 8.0, 0.0),
            &linear_falloff(),
        );
        let tile = c.tile((0, 0)).expect("tile allocated");
        assert_eq!(tile.texel(100, 100), [0.0, 0.0, 0.0, 1.0]);
    }

    /// Pixels outside the radius must be untouched -- the dab must not leak past its own
    /// bounds. Untouched means *transparent* now that a canvas is one layer.
    #[test]
    fn a_dab_does_not_paint_outside_its_radius() {
        let mut c = Canvas::new(256, 256);
        rasterize_dab(
            &mut c,
            &black_dab(100.5, 100.5, 8.0, 0.0),
            &linear_falloff(),
        );
        let tile = c.tile((0, 0)).expect("tile allocated");
        // 12px away, well beyond radius 8.
        assert_eq!(tile.texel(112, 100), [0.0; 4]);
    }

    #[test]
    fn a_dab_spanning_a_tile_seam_touches_both_tiles() {
        let mut c = Canvas::new(1024, 1024);
        // Centered exactly on the boundary between tile column 0 and 1.
        rasterize_dab(
            &mut c,
            &black_dab(TILE_SIZE as f32, 100.0, 8.0, 0.5),
            &linear_falloff(),
        );
        assert_eq!(c.tiles().count(), 2);
    }

    #[test]
    fn dabs_off_canvas_are_clipped_not_panicking() {
        let mut c = Canvas::new(64, 64);
        rasterize_dab(
            &mut c,
            &black_dab(-50.0, -50.0, 8.0, 0.5),
            &linear_falloff(),
        );
        assert_eq!(c.tiles().count(), 0);
        // Straddling the origin should paint only the in-bounds part.
        rasterize_dab(&mut c, &black_dab(0.0, 0.0, 8.0, 0.5), &linear_falloff());
        assert_eq!(c.tiles().count(), 1);
    }

    #[test]
    fn batch_rasterization_matches_one_at_a_time() {
        let dabs = [
            black_dab(50.0, 50.0, 6.0, 0.5),
            black_dab(56.0, 50.0, 6.0, 0.5),
            black_dab(62.0, 50.0, 6.0, 0.5),
        ];

        let mut batched = Canvas::new(256, 256);
        rasterize_dabs(&mut batched, &dabs, &linear_falloff());

        let mut individually = Canvas::new(256, 256);
        for d in &dabs {
            rasterize_dab(&mut individually, d, &linear_falloff());
        }

        let a = batched.tile((0, 0)).expect("tile");
        let b = individually.tile((0, 0)).expect("tile");
        for y in 40..60 {
            for x in 40..70 {
                assert_eq!(a.texel(x, y), b.texel(x, y), "differ at ({x},{y})");
            }
        }
    }
}
