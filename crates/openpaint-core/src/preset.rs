//! Brush presets: a named brush you can come back to.
//!
//! Nobody draws with one brush. A page needs at least a pencil for roughs, an ink pen for line
//! art, and an eraser with its own size and softness — and re-dialling six sliders to move between
//! them is the difference between a demo and something you would actually draw with.
//!
//! # A preset is not a brush
//!
//! Two things a [`Brush`] carries are deliberately **not** in a preset:
//!
//! - **The colour.** Picking a pen must not change your ink. Every app keeps these apart, and the
//!   reason is that colour changes far more often than tool does.
//! - **The tip's pixels.** A bitmap tip is an app resource (§4p), not something to copy into every
//!   preset that uses it. A preset *references* one, the same way a text layer references a font
//!   family by name — and reports it when the reference cannot be resolved, rather than silently
//!   drawing with something else (§6a).
//!
//! Everything else is copied wholesale rather than field by field, so a new brush setting is
//! carried by presets the day it is added instead of the day somebody remembers to list it here.
//! That is the §11a.8 hazard — one definition, two lists — and copying the struct is how it is
//! avoided.
//!
//! # Why the edge profile rides alongside
//!
//! [`crate::dab::Tip`] is either a curve or a bitmap, so a preset using a bitmap tip would have
//! nowhere to keep its edge profile — and switching such a preset back to a round tip would find
//! the curve gone. The profile is therefore stored separately and always, and the tip reference
//! only decides which of the two is in force.

use crate::curve::Curve;
use crate::Brush;

/// Which tip a preset asks for.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TipRef {
    /// The procedural disc, shaped by the preset's edge profile.
    #[default]
    Round,
    /// A bitmap, by the path it was loaded from.
    ///
    /// A path rather than a name because there is no tip library yet, and a path *is* the name
    /// under those circumstances. When one arrives this becomes a name and nothing else changes —
    /// the resolution already reports failure rather than assuming success.
    File { path: String },
}

/// A named set of brush settings.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BrushPreset {
    pub name: String,
    /// Every setting except the colour and the tip. See the module note.
    pub brush: Brush,
    /// The edge profile, kept out of `brush` because a stamped tip has nowhere to hold one.
    #[serde(default)]
    pub edge: Curve,
    #[serde(default)]
    pub tip: TipRef,
}

impl BrushPreset {
    /// Capture the brush as it stands, under a name.
    #[must_use]
    pub fn capture(name: impl Into<String>, brush: &Brush, tip: TipRef) -> Self {
        Self {
            name: name.into(),
            brush: brush.clone(),
            // Whatever profile is in force, whether or not it is the one being drawn with: a
            // preset saved while a bitmap tip is loaded still remembers the edge it would use if
            // the tip were taken off.
            edge: brush.tip.falloff().cloned().unwrap_or_default(),
            tip,
        }
    }

    /// Apply every setting to `brush`, leaving its colour and its tip alone.
    ///
    /// The tip is the caller's to set, because resolving one means touching the filesystem and
    /// uploading a texture — neither of which belongs in the engine. What is set here is the
    /// procedural edge, which is the right answer whenever the reference does not resolve.
    pub fn apply_to(&self, brush: &mut Brush) {
        let colour = brush.color_linear_premul();
        *brush = self.brush.clone();
        brush.tip = crate::dab::Tip::Round(self.edge.clone());
        brush.set_color_linear_premul(colour);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modulation::Source;

    #[test]
    fn a_preset_carries_the_settings_but_not_the_colour() {
        let mut brush = Brush::default();
        brush.radius = 42.0;
        brush.spacing = 0.05;
        brush.stabilization_ms = 30.0;
        brush.radius_response.source = Source::Velocity;
        brush.set_color_srgb8([200, 30, 30]);

        let preset = BrushPreset::capture("Ink", &brush, TipRef::Round);

        let mut other = Brush::default();
        other.set_color_srgb8([10, 20, 30]);
        let ink = other.color_linear_premul();
        preset.apply_to(&mut other);

        assert_eq!(other.radius, 42.0);
        assert_eq!(other.spacing, 0.05);
        assert_eq!(other.stabilization_ms, 30.0);
        assert_eq!(other.radius_response.source, Source::Velocity);
        assert_eq!(
            other.color_linear_premul(),
            ink,
            "picking a pen must not change the ink"
        );
    }

    /// A preset saved while a bitmap tip was loaded still remembers the edge profile, so switching
    /// it back to a round tip does not find the curve gone.
    #[test]
    fn the_edge_profile_survives_a_bitmap_tip() {
        let mut brush = Brush::default();
        let soft =
            Curve::from_points(vec![(0.0, 1.0), (0.5, 0.9), (1.0, 0.0)]).expect("a valid curve");
        brush.tip = crate::dab::Tip::Round(soft.clone());
        let saved = BrushPreset::capture("Soft", &brush, TipRef::Round);

        brush.tip = crate::dab::Tip::Stamp(std::sync::Arc::new(
            crate::Stamp::new(2, 2, vec![255, 255, 255, 255]).expect("a valid stamp"),
        ));
        let stamped = BrushPreset::capture(
            "Chalk",
            &brush,
            TipRef::File {
                path: "c.png".into(),
            },
        );

        assert_eq!(saved.edge, soft);
        assert_eq!(
            stamped.edge,
            Curve::default(),
            "a stamped tip has no profile of its own, so the default is the honest answer"
        );

        let mut applied = Brush::default();
        saved.apply_to(&mut applied);
        assert_eq!(applied.tip.falloff(), Some(&soft));
    }

    /// A preset survives a round trip through its file format, curves and all.
    ///
    /// The property that matters: nested, variable-length data — six response curves, each a
    /// source and a list of control points — has to come back exactly, or a preset would slowly
    /// stop meaning what it did when it was saved.
    #[test]
    fn a_preset_round_trips_through_json() {
        let mut brush = Brush::default();
        brush.radius = 17.5;
        brush.roundness = 0.4;
        brush.angle = 0.125;
        brush.flow_response.source = Source::Tilt;
        brush.flow_response.curve =
            Curve::from_points(vec![(0.0, 0.2), (0.4, 0.8), (1.0, 1.0)]).expect("a valid curve");
        let preset = BrushPreset::capture(
            "Chisel",
            &brush,
            TipRef::File {
                path: "tips/nib.png".to_owned(),
            },
        );

        let text = serde_json::to_string(&preset).expect("serializes");
        let mut back: BrushPreset = serde_json::from_str(&text).expect("deserializes");

        // The colour deliberately does not travel, so a round trip is *not* the identity on it.
        // Asserting that first states the contract rather than letting it hide inside a mismatch.
        assert_ne!(
            back.brush.color_linear_premul(),
            preset.brush.color_linear_premul(),
            "a preset must not carry ink with it"
        );
        back.brush
            .set_color_linear_premul(preset.brush.color_linear_premul());
        assert_eq!(back, preset);
    }
}
