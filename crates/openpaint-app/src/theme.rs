//! The look, as data.
//!
//! Every colour and every measurement the UI uses lives here and nowhere else. That is the rule
//! that makes §1b's third goal reachable: if a widget never contains a literal, then restyling is
//! editing a file and "make the spacing a setting" is publishing a knob that already exists rather
//! than adding one.
//!
//! **The discipline, stated once so it can be checked against:** a widget reads a token; it never
//! writes a colour or a size of its own. The moment one does, the look stops being data and starts
//! being spread across the code — which is the same failure as the layout knowing what a panel is,
//! one level up.
//!
//! # Roles, not names
//!
//! Tokens are named for the job they do — `state`, `dim`, `edge` — rather than for what they look
//! like. `blue` would be a lie in the warm variant, and a second theme would either rename
//! everything or keep a field called `blue` holding a sienna.
//!
//! # Colours are hex text on purpose
//!
//! A theme is meant to be readable and hand-editable — that is most of the point of it being a
//! file. `"#6AA0D8"` is a colour anyone can change; `[106, 160, 216, 255]` is a puzzle.
//!
//! # Logical units
//!
//! Every measurement is logical, matching [`crate::layout`]. The display scale is applied once at
//! draw time. See that module for why this is not a detail to settle later.

// Built ahead of the shell that will draw it, like `crate::layout`. `expect` rather than `allow`
// so it becomes an error the moment the shell starts calling it.
#![expect(dead_code, reason = "the theme lands before the widgets that read it")]

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// An opaque sRGB colour, written as `#RRGGBB`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color(pub [u8; 3]);

impl Color {
    #[must_use]
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self([r, g, b])
    }

    /// This colour at `alpha`, for overlays drawn over the artwork.
    #[must_use]
    pub fn with_alpha(self, alpha: u8) -> [u8; 4] {
        [self.0[0], self.0[1], self.0[2], alpha]
    }

    /// Relative luminance, for the contrast check.
    #[must_use]
    fn luminance(self) -> f32 {
        let channel = |c: u8| {
            let c = f32::from(c) / 255.0;
            if c <= 0.040_45 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.0722f32.mul_add(
            channel(self.0[2]),
            0.2126f32.mul_add(channel(self.0[0]), 0.7152 * channel(self.0[1])),
        )
    }

    /// WCAG contrast ratio against another colour, from 1 (identical) to 21 (black on white).
    #[must_use]
    pub fn contrast(self, other: Self) -> f32 {
        let (a, b) = (self.luminance(), other.luminance());
        let (hi, lo) = if a > b { (a, b) } else { (b, a) };
        (hi + 0.05) / (lo + 0.05)
    }
}

impl fmt::Display for Color {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{:02X}{:02X}{:02X}", self.0[0], self.0[1], self.0[2])
    }
}

impl Serialize for Color {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Color {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        let hex = text.strip_prefix('#').unwrap_or(&text);
        if hex.len() != 6 {
            return Err(serde::de::Error::custom(format!(
                "a colour is six hex digits like #6AA0D8, got {text:?}"
            )));
        }
        let byte = |i: usize| {
            u8::from_str_radix(&hex[i..i + 2], 16)
                .map_err(|_| serde::de::Error::custom(format!("{text:?} is not hexadecimal")))
        };
        Ok(Self([byte(0)?, byte(2)?, byte(4)?]))
    }
}

/// The colours the UI draws with, named for what they do.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Palette {
    /// The window ground, seen between panels.
    pub ground: Color,
    /// A panel's body.
    pub panel: Color,
    /// A panel's header strip.
    pub header: Color,
    /// Hairlines, dividers, and the edge of things.
    pub edge: Color,
    /// Ordinary text.
    pub text: Color,
    /// Secondary text: panel names, units, anything the eye should pass over.
    pub dim: Color,
    /// Emphasis: the active tab, a value being changed.
    pub bright: Color,
    /// **The one accent.** Active, selected, being dragged — state and nothing else, so the eye
    /// goes to what is happening rather than to decoration.
    pub state: Color,
    /// Text drawn *on* the accent, for a selected row that inverts.
    ///
    /// Its own role rather than reusing `bright`, because which of dark or light reads on an
    /// accent depends entirely on the accent: near-white on the default's mid blue measures
    /// 2.4:1 and cannot be read, while near-black on it is comfortable — and the warm theme's
    /// darker sienna wants the opposite. A test caught this; the eye it was written for could not.
    pub on_state: Color,
    /// The void the page sits on.
    pub canvas: Color,
}

/// Sizes and spacings, in logical units.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Metrics {
    /// The gap between panels, through which the ground shows.
    ///
    /// How the structure of the tree is read at a glance, without drawing a border around
    /// everything — which is why it is a gap rather than a stroke.
    pub gutter: f32,
    /// Height of a panel's header.
    ///
    /// Sized for a pen rather than a pointer: this is the surface you press and hold to move a
    /// panel, so it is a target before it is a label.
    pub header: f32,
    /// A compact header, for a panel with nothing worth captioning.
    ///
    /// Still a grab surface, just one that spends no room on a name — a header *style* available
    /// to any panel, not an exception carved out for the toolbar (§1c).
    pub header_compact: f32,
    /// Space either side of a tab's label.
    pub tab_padding: f32,
    /// Space inside a panel, around its content.
    pub padding: f32,
    /// How wide a divider is to grab, which is wider than the gutter it sits in.
    pub splitter_grab: f32,
    /// Corner rounding on panels.
    pub radius: f32,
    /// Text size for panel names and tabs.
    pub label: f32,
    /// Text size for ordinary content.
    pub body: f32,
}

/// A complete look.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Theme {
    pub palette: Palette,
    pub metrics: Metrics,
}

impl Default for Theme {
    /// The dark, cool default — "Studio" from the mockups.
    ///
    /// Neutrals are biased slightly toward blue rather than being pure grey, so the accent belongs
    /// to the same family as the surfaces around it instead of sitting on top of them.
    fn default() -> Self {
        Self {
            palette: Palette {
                ground: Color::rgb(0x17, 0x1A, 0x1E),
                panel: Color::rgb(0x1D, 0x21, 0x26),
                header: Color::rgb(0x23, 0x28, 0x2E),
                edge: Color::rgb(0x2C, 0x32, 0x3A),
                text: Color::rgb(0xC6, 0xCC, 0xD3),
                dim: Color::rgb(0x7C, 0x85, 0x8F),
                bright: Color::rgb(0xED, 0xF1, 0xF5),
                state: Color::rgb(0x6A, 0xA0, 0xD8),
                on_state: Color::rgb(0x12, 0x15, 0x19),
                canvas: Color::rgb(0x0D, 0x0F, 0x11),
            },
            metrics: Metrics {
                gutter: 2.0,
                header: 24.0,
                header_compact: 8.0,
                tab_padding: 10.0,
                padding: 8.0,
                splitter_grab: 10.0,
                radius: 3.0,
                label: 11.0,
                body: 12.0,
            },
        }
    }
}

impl Theme {
    /// The warm light variant — "Paper" from the mockups.
    ///
    /// Here to prove the point rather than to be the default: it differs from [`Theme::default`]
    /// in nine colours and nothing else, which is what "the styles are data" has to mean if it
    /// means anything.
    #[must_use]
    pub fn paper() -> Self {
        Self {
            palette: Palette {
                ground: Color::rgb(0xDC, 0xD8, 0xD0),
                panel: Color::rgb(0xF4, 0xF2, 0xED),
                header: Color::rgb(0xE7, 0xE3, 0xDB),
                edge: Color::rgb(0xD2, 0xCD, 0xC3),
                text: Color::rgb(0x3A, 0x37, 0x2F),
                dim: Color::rgb(0x7D, 0x77, 0x6B),
                bright: Color::rgb(0x26, 0x24, 0x1F),
                state: Color::rgb(0x9A, 0x5B, 0x3D),
                on_state: Color::rgb(0xFB, 0xF9, 0xF5),
                canvas: Color::rgb(0xCF, 0xCA, 0xC0),
            },
            metrics: Theme::default().metrics,
        }
    }

    /// Read a theme from JSON, so a look is a file rather than a build.
    ///
    /// # Errors
    /// If the text is not a theme.
    pub fn from_json(text: &str) -> Result<Self, String> {
        serde_json::from_str(text).map_err(|e| e.to_string())
    }

    /// Write it back out, formatted to be edited by hand.
    ///
    /// # Errors
    /// If serialisation fails, which for this type it cannot.
    pub fn to_json(self) -> Result<String, String> {
        serde_json::to_string_pretty(&self).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A theme survives being written and read, and the file is the readable kind.
    #[test]
    fn a_theme_round_trips_as_editable_text() {
        let theme = Theme::default();
        let text = theme.to_json().expect("serialises");
        assert!(
            text.contains("\"#6AA0D8\""),
            "colours must be hex a person can edit, got:\n{text}"
        );
        assert_eq!(Theme::from_json(&text).expect("deserialises"), theme);
    }

    /// A malformed colour is refused with a message that says what a colour looks like — rather
    /// than defaulting to black and leaving someone to wonder why their accent vanished (§6b).
    #[test]
    fn a_bad_colour_is_refused_and_explains_itself() {
        let broken = Theme::default()
            .to_json()
            .expect("serialises")
            .replace("#6AA0D8", "kingfisher");
        let err = Theme::from_json(&broken).expect_err("that is not a colour");
        assert!(
            err.contains("six hex digits"),
            "the message has to teach the format, got {err:?}"
        );
    }

    /// Text has to be legible on the surface behind it, in **every** theme. A palette is a set,
    /// and checking only the default would let the second one ship unreadable.
    ///
    /// 4.5 is the WCAG threshold for body text; the dim role carries labels and units, so it is
    /// held to 3.0 — the large-text threshold — deliberately rather than by omission.
    #[test]
    fn every_theme_keeps_its_text_legible() {
        for (name, theme) in [("default", Theme::default()), ("paper", Theme::paper())] {
            let p = theme.palette;
            for (what, fg, bg) in [
                ("text on panel", p.text, p.panel),
                ("bright on panel", p.bright, p.panel),
                ("text on header", p.text, p.header),
                ("bright on header", p.bright, p.header),
            ] {
                let ratio = fg.contrast(bg);
                assert!(
                    ratio >= 4.5,
                    "{name}: {what} is {ratio:.2}:1, below the 4.5 needed to read"
                );
            }
            for (what, fg, bg) in [
                ("dim on panel", p.dim, p.panel),
                ("dim on header", p.dim, p.header),
            ] {
                let ratio = fg.contrast(bg);
                assert!(ratio >= 3.0, "{name}: {what} is {ratio:.2}:1, below 3.0");
            }
        }
    }

    /// The accent has to read as *state* against the surfaces it marks, or "selected" becomes a
    /// guess. Checked in both themes, since an accent that works on dark often disappears on light.
    #[test]
    fn the_accent_stands_out_in_every_theme() {
        for (name, theme) in [("default", Theme::default()), ("paper", Theme::paper())] {
            let p = theme.palette;
            let against_panel = p.state.contrast(p.panel);
            assert!(
                against_panel >= 2.5,
                "{name}: the accent is {against_panel:.2}:1 against a panel and will not read"
            );
            // And a label on top of the accent has to survive too, since selected rows invert.
            // Held to the full 4.5: a selected layer's name is body text, and being selected is
            // not a reason to be harder to read.
            let on_state = p.on_state.contrast(p.state);
            assert!(
                on_state >= 4.5,
                "{name}: text on the accent is {on_state:.2}:1, below the 4.5 needed to read"
            );
        }
    }

    /// The two themes differ in colour and *only* in colour. That is the claim the mockups made
    /// and this is what holds it true: if a future theme starts changing metrics, this fails and
    /// the claim gets rewritten rather than quietly becoming false.
    #[test]
    fn the_themes_differ_only_in_their_palette() {
        assert_ne!(Theme::default().palette, Theme::paper().palette);
        assert_eq!(Theme::default().metrics, Theme::paper().metrics);
    }

    /// A header is a grab target before it is a label, so it has to be big enough to press with a
    /// pen. Forty-four is the usual touch minimum; a pen is more precise than a fingertip, but not
    /// so much that a 12-unit strip is fair.
    #[test]
    fn a_header_is_big_enough_to_grab() {
        let m = Theme::default().metrics;
        assert!(m.header >= 20.0, "a full header is {} units", m.header);
        assert!(
            m.splitter_grab > m.gutter,
            "a divider must be wider to grab than the line it draws, or it cannot be caught"
        );
    }
}
