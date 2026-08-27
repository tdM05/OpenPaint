//! Stamp-based round brush.
//!
//! This is the architectural seed of the real brush engine: a stroke is not a
//! polyline, it's a series of round *dabs* stamped along the path at a fixed
//! fraction of the brush diameter (the "spacing", ~25% in Photoshop). Each dab
//! has a soft falloff from center to edge (hardness). Building it this way now
//! means the path toward Photoshop/CSP-quality brushes is incremental tuning,
//! not a rewrite.
//!
//! Phase 0 scope: constant color, sRGB-space coverage blend, pressure maps to
//! dab radius. Flow/opacity accumulation, linear-space blending, textures, and
//! tuned falloff curves come with the real engine (see docs Q7a).

use crate::canvas::Canvas;

/// Brush parameters. Sizes are in canvas pixels.
#[derive(Clone, Copy)]
pub struct Brush {
    /// Dab radius at full pressure, in pixels.
    pub radius: f32,
    /// Edge softness in `0.0..=1.0`: 0 = hard edge, 1 = fully soft falloff.
    pub hardness: f32,
    /// Dab spacing as a fraction of diameter (Photoshop default ≈ 0.25).
    pub spacing: f32,
    /// Brush color, straight RGB.
    pub color: [u8; 3],
}

impl Default for Brush {
    fn default() -> Self {
        Self {
            radius: 8.0,
            hardness: 0.5,
            spacing: 0.25,
            color: [20, 20, 24],
        }
    }
}

/// Tracks stroke continuity so dabs are spaced correctly across successive
/// input samples (which arrive at irregular distances). Create one per stroke.
pub struct StrokeState {
    /// Last sample position, and how far we've traveled since the last dab.
    last: Option<(f32, f32)>,
    /// Distance accumulated since the previous stamped dab.
    residual: f32,
}

impl StrokeState {
    pub fn new() -> Self {
        Self {
            last: None,
            residual: 0.0,
        }
    }
}

impl Default for StrokeState {
    fn default() -> Self {
        Self::new()
    }
}

impl Brush {
    /// Effective dab radius for a given pressure in `0.0..=1.0`.
    fn radius_for(&self, pressure: f32) -> f32 {
        // Simple linear pressure→size for now. A configurable response curve
        // arrives with the real engine.
        (self.radius * pressure.clamp(0.0, 1.0)).max(0.5)
    }

    /// Stamp one dab centered at `(cx, cy)` with the given radius.
    fn stamp(&self, canvas: &mut Canvas, cx: f32, cy: f32, radius: f32) {
        if radius <= 0.0 {
            return;
        }
        let min_x = (cx - radius).floor() as i32;
        let max_x = (cx + radius).ceil() as i32;
        let min_y = (cy - radius).floor() as i32;
        let max_y = (cy + radius).ceil() as i32;

        // hardness=1 -> falloff starts at the very center (very soft);
        // hardness=0 -> solid until ~1px from the edge (hard, still AA'd).
        let inner = (radius * (1.0 - self.hardness)).max(0.0);

        for y in min_y..=max_y {
            for x in min_x..=max_x {
                let dx = x as f32 + 0.5 - cx;
                let dy = y as f32 + 0.5 - cy;
                let dist = (dx * dx + dy * dy).sqrt();
                let coverage = if dist <= inner {
                    1.0
                } else if dist >= radius {
                    0.0
                } else {
                    // Smooth ramp from inner..radius.
                    1.0 - (dist - inner) / (radius - inner)
                };
                if coverage > 0.0 {
                    canvas.blend_pixel(x, y, self.color, coverage);
                }
            }
        }
    }

    /// Begin a stroke: stamp the initial dab at the first sample.
    pub fn stroke_begin(
        &self,
        canvas: &mut Canvas,
        state: &mut StrokeState,
        x: f32,
        y: f32,
        pressure: f32,
    ) {
        self.stamp(canvas, x, y, self.radius_for(pressure));
        state.last = Some((x, y));
        state.residual = 0.0;
    }

    /// Continue a stroke to a new sample, stamping evenly spaced dabs along the
    /// segment from the previous sample so speed doesn't create gaps.
    pub fn stroke_to(
        &self,
        canvas: &mut Canvas,
        state: &mut StrokeState,
        x: f32,
        y: f32,
        pressure: f32,
    ) {
        let Some((px, py)) = state.last else {
            self.stroke_begin(canvas, state, x, y, pressure);
            return;
        };

        let radius = self.radius_for(pressure);
        let step = (radius * 2.0 * self.spacing).max(0.5);

        let dx = x - px;
        let dy = y - py;
        let seg_len = (dx * dx + dy * dy).sqrt();
        if seg_len <= f32::EPSILON {
            return;
        }
        let (ux, uy) = (dx / seg_len, dy / seg_len);

        // Walk along the segment, emitting a dab every `step` pixels, carrying
        // leftover distance in `residual` so spacing is continuous across
        // segments.
        let mut traveled = -state.residual;
        while traveled + step <= seg_len {
            traveled += step;
            let t = traveled;
            self.stamp(canvas, px + ux * t, py + uy * t, radius);
        }
        state.residual = seg_len - traveled;
        state.last = Some((x, y));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dab_paints_something() {
        let mut c = Canvas::new(256, 256);
        let b = Brush::default();
        let mut s = StrokeState::new();
        b.stroke_begin(&mut c, &mut s, 128.0, 128.0, 1.0);
        assert!(c.tiles().count() >= 1);
    }

    #[test]
    fn a_stroke_spans_multiple_tiles() {
        let mut c = Canvas::new(1024, 1024);
        let b = Brush::default();
        let mut s = StrokeState::new();
        b.stroke_begin(&mut c, &mut s, 10.0, 10.0, 1.0);
        b.stroke_to(&mut c, &mut s, 600.0, 10.0, 1.0);
        assert!(c.tiles().count() >= 2);
    }
}
