//! Color space and alpha conventions.
//!
//! Two rules hold everywhere inside the engine, per `docs/DECISIONS.md` §4b:
//!
//! 1. **Pixels are linear**, not sRGB-encoded. Compositing sRGB-encoded values
//!    is simply wrong — it darkens gradients and puts halos on antialiased
//!    edges. sRGB exists only at the boundaries: authored colors coming in, and
//!    the final blit to the display going out.
//! 2. **Alpha is premultiplied.** Straight alpha needs a divide before every
//!    filter or blend and misbehaves wherever alpha is 0, which breaks layer
//!    filtering, masks, and blend modes in ways that are tedious to chase down.
//!
//! Both conventions are cheap to hold now and expensive to retrofit, which is
//! why they are established before the brush engine is written.

/// Convert one sRGB-encoded channel (`0.0..=1.0`) to linear.
///
/// This is the real piecewise sRGB transfer function, not the `pow(x, 2.2)`
/// approximation — the linear segment near black matters for dark strokes, and
/// getting it wrong shows up as muddy shadows.
#[must_use]
pub fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Convert one linear channel (`0.0..=1.0`) to sRGB-encoded.
#[must_use]
pub fn linear_to_srgb(c: f32) -> f32 {
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// Convert an 8-bit sRGB channel to linear.
#[must_use]
pub fn srgb8_to_linear(c: u8) -> f32 {
    srgb_to_linear(f32::from(c) / 255.0)
}

/// Convert an authored opaque sRGB color to a linear, premultiplied RGBA value.
///
/// Opaque, so premultiplication is a no-op on the color channels; the helper
/// exists so call sites never have to remember that.
#[must_use]
pub fn opaque_srgb8_to_linear_premul(rgb: [u8; 3]) -> [f32; 4] {
    [
        srgb8_to_linear(rgb[0]),
        srgb8_to_linear(rgb[1]),
        srgb8_to_linear(rgb[2]),
        1.0,
    ]
}

/// Scale a linear premultiplied color by a coverage/alpha factor.
///
/// Because the value is premultiplied, coverage scales **all four** channels —
/// that is precisely what makes premultiplied blending a plain lerp instead of a
/// special case.
#[must_use]
pub fn scale_premul(premul: [f32; 4], factor: f32) -> [f32; 4] {
    [
        premul[0] * factor,
        premul[1] * factor,
        premul[2] * factor,
        premul[3] * factor,
    ]
}

/// Composite linear premultiplied `src` over linear premultiplied `dst`
/// (Porter-Duff "over").
///
/// With premultiplied inputs this is just `src + dst * (1 - src.a)`, with no
/// divide and no zero-alpha special case.
#[must_use]
pub fn over_premul(src: [f32; 4], dst: [f32; 4]) -> [f32; 4] {
    let inv = 1.0 - src[3];
    [
        src[0] + dst[0] * inv,
        src[1] + dst[1] * inv,
        src[2] + dst[2] * inv,
        src[3] + dst[3] * inv,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srgb_roundtrips() {
        for i in 0..=255u8 {
            let lin = srgb8_to_linear(i);
            let back = (linear_to_srgb(lin) * 255.0).round() as u8;
            assert_eq!(back, i, "roundtrip failed at {i}");
        }
    }

    #[test]
    fn srgb_endpoints_are_exact() {
        assert!((srgb_to_linear(0.0)).abs() < 1e-6);
        assert!((srgb_to_linear(1.0) - 1.0).abs() < 1e-6);
    }

    /// Mid-grey sRGB is famously ~0.216 in linear, not 0.5. If this drifts, the
    /// transfer function is wrong and every blend is subtly off.
    #[test]
    fn mid_grey_is_not_half_in_linear() {
        let lin = srgb8_to_linear(128);
        assert!((lin - 0.2159).abs() < 0.001, "got {lin}");
    }

    #[test]
    fn over_opaque_src_replaces_dst() {
        let src = [0.25, 0.5, 0.75, 1.0];
        let dst = [1.0, 1.0, 1.0, 1.0];
        assert_eq!(over_premul(src, dst), src);
    }

    #[test]
    fn over_transparent_src_keeps_dst() {
        let dst = [0.25, 0.5, 0.75, 1.0];
        assert_eq!(over_premul([0.0; 4], dst), dst);
    }

    /// Half-coverage black over white must land at half in *linear* space.
    #[test]
    fn half_coverage_blends_linearly() {
        let src = scale_premul([0.0, 0.0, 0.0, 1.0], 0.5);
        let dst = [1.0, 1.0, 1.0, 1.0];
        let out = over_premul(src, dst);
        assert!((out[0] - 0.5).abs() < 1e-6, "got {}", out[0]);
        assert!((out[3] - 1.0).abs() < 1e-6);
    }
}
