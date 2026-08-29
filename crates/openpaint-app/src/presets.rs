//! The brush library: presets on disk, and resolving the tips they reference.
//!
//! A brush is a **tool you own**, not part of the artwork — the same call §4p made for bitmap tips,
//! and for the same reason. Nothing about a preset is written into a `.openpaint` file, so a
//! document stays openable on a machine that has never heard of your inking pen.
//!
//! # One file, read and written whole
//!
//! `brushes.json` beside the recovery copies, in the OS's per-user data directory. JSON rather
//! than another SQLite container because a preset is nested, variable-length data — six response
//! curves, each a source and a list of control points — read all at once and written all at once.
//! SQLite earns its place in `.openpaint` by holding thousands of tiles with random access; there
//! is nothing here for it to do. That the file is readable and hand-editable is a real feature the
//! day somebody wants to share a brush.
//!
//! # Failure is reported, never silent
//!
//! A library that will not load, a preset that will not save, a tip file that has gone: each
//! returns something the caller can say out loud (§6b). The one thing that never happens is a
//! brush quietly drawing with a tip that is not the one it names — a missing tip falls back to the
//! procedural edge *and says so*, exactly as a missing font does (§6a).

use std::path::{Path, PathBuf};

use openpaint_core::{BrushPreset, TipRef};

/// Where the library lives, created if needed.
///
/// Beside the recovery copies rather than next to any document: the folder a document sits in may
/// be read-only, on removable media, or synced — none of which is true of the place the OS sets
/// aside for exactly this.
fn library_path() -> Option<PathBuf> {
    let dir = dirs::data_local_dir()?.join("OpenPaint");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("brushes.json"))
}

/// The presets on disk, and what went wrong reading them.
#[derive(Debug, Default)]
pub struct Library {
    presets: Vec<BrushPreset>,
    /// Set when the file exists but could not be read, so the app can say so instead of appearing
    /// to have lost every brush.
    pub trouble: Option<String>,
}

impl Library {
    /// Read the library, or start an empty one.
    ///
    /// A missing file is not an error — it is what a first run looks like. A *corrupt* one is, and
    /// the presets are left empty rather than partially loaded: half a library silently is worse
    /// than none loudly.
    #[must_use]
    pub fn load() -> Self {
        let Some(path) = library_path() else {
            return Self {
                presets: Vec::new(),
                trouble: Some("No writable data directory, so brushes cannot be saved.".to_owned()),
            };
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<Vec<BrushPreset>>(&text) {
                Ok(presets) => Self {
                    presets,
                    trouble: None,
                },
                Err(e) => Self {
                    presets: Vec::new(),
                    trouble: Some(format!("{} could not be read: {e}", path.display())),
                },
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => Self {
                presets: Vec::new(),
                trouble: Some(format!("{} could not be opened: {e}", path.display())),
            },
        }
    }

    #[must_use]
    pub fn presets(&self) -> &[BrushPreset] {
        &self.presets
    }

    /// Add a preset, replacing any with the same name. Returns whether it replaced one.
    ///
    /// Replacing rather than duplicating, because saving under a name you already used means
    /// "update that one" everywhere else that offers this.
    ///
    /// Separate from [`Library::save`] so the decision can be tested without a disk: what has
    /// logic in it is *this*, and a test that reached for `save` would have to reimplement it or
    /// write to the artist's real library. Both are worse.
    fn insert(&mut self, preset: BrushPreset) -> bool {
        match self.presets.iter_mut().find(|p| p.name == preset.name) {
            Some(existing) => {
                *existing = preset;
                true
            }
            None => {
                self.presets.push(preset);
                false
            }
        }
    }

    /// Add a preset and write the library out. Returns what to tell the artist.
    pub fn save(&mut self, preset: BrushPreset) -> String {
        let name = preset.name.clone();
        let replaced = self.insert(preset);
        match self.write() {
            Ok(()) if replaced => format!("Updated the brush \"{name}\""),
            Ok(()) => format!("Saved the brush \"{name}\""),
            Err(e) => format!("\"{name}\" is available now, but could not be saved: {e}"),
        }
    }

    /// Take a preset out, returning its name. `None` if there was no such preset.
    ///
    /// Split from [`Library::remove`] for the same reason [`Library::insert`] is: this is the part
    /// with a decision in it, and a test that reached for `remove` would either write to the
    /// artist's real library or reimplement this — and a test that reimplements what it is testing
    /// tests nothing.
    fn take(&mut self, index: usize) -> Option<String> {
        (index < self.presets.len()).then(|| self.presets.remove(index).name)
    }

    /// Forget a preset and write the library out. Returns what to tell the artist, or `None` if
    /// there was no such preset.
    pub fn remove(&mut self, index: usize) -> Option<String> {
        let name = self.take(index)?;
        Some(match self.write() {
            Ok(()) => format!("Deleted the brush \"{name}\""),
            Err(e) => format!(
                "\"{name}\" is gone from this session, but the file could not be updated: {e}"
            ),
        })
    }

    /// Write the library out.
    ///
    /// Through a temporary file and a rename, so an interrupted write cannot leave a half-written
    /// library where a whole one was. The same reasoning as autosave's: the failure that matters is
    /// not "the save did not happen", it is "the save destroyed what was there".
    fn write(&self) -> Result<(), String> {
        let Some(path) = library_path() else {
            return Err("no writable data directory".to_owned());
        };
        let text = serde_json::to_string_pretty(&self.presets).map_err(|e| e.to_string())?;
        let temp = path.with_extension("json.tmp");
        std::fs::write(&temp, text).map_err(|e| e.to_string())?;
        std::fs::rename(&temp, &path).map_err(|e| e.to_string())
    }
}

/// What resolving a preset's tip produced.
pub enum Resolved {
    /// The preset asked for the procedural edge, and got it.
    Round,
    /// A bitmap tip, loaded.
    Stamp(openpaint_core::Stamp),
    /// A bitmap tip was asked for and could not be had. Carries what to say about it.
    Missing(String),
}

/// Load the tip a preset names.
///
/// Reported rather than hidden, per §6a: a preset drawn with the wrong tip looks like a bug in the
/// brush engine, and the artist has no way to guess that a file moved.
#[must_use]
pub fn resolve_tip(
    tip: &TipRef,
    read: impl Fn(&Path) -> Result<openpaint_core::Stamp, String>,
) -> Resolved {
    match tip {
        TipRef::Round => Resolved::Round,
        TipRef::File { path } => match read(Path::new(path)) {
            Ok(stamp) => Resolved::Stamp(stamp),
            Err(e) => Resolved::Missing(format!(
                "This brush wants the tip {path}, which could not be loaded ({e}). Drawing with a \
                 round tip instead."
            )),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preset(name: &str) -> BrushPreset {
        BrushPreset::capture(name, &openpaint_core::Brush::default(), TipRef::Round)
    }

    /// Saving under a name already in use updates that preset rather than growing a second one
    /// with the same name — which is what every app means by saving over something.
    #[test]
    fn saving_over_a_name_replaces_it() {
        let mut library = Library::default();
        assert!(
            !library.insert(preset("Ink")),
            "the first is not a replacement"
        );
        library.insert(preset("Pencil"));

        let mut updated = preset("Ink");
        updated.brush.radius = 99.0;
        assert!(library.insert(updated), "the second under that name is");

        assert_eq!(library.presets().len(), 2, "no second \"Ink\" was added");
        assert_eq!(library.presets()[0].brush.radius, 99.0);
        assert_eq!(library.presets()[1].name, "Pencil", "order is preserved");
    }

    /// Removing takes the right one out and leaves the rest in order.
    #[test]
    fn removing_takes_only_that_preset() {
        let mut library = Library::default();
        for name in ["Ink", "Pencil", "Chalk"] {
            library.insert(preset(name));
        }
        assert_eq!(library.take(1).as_deref(), Some("Pencil"));
        let names: Vec<&str> = library.presets().iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Ink", "Chalk"]);
        assert_eq!(library.take(9), None, "an index past the end takes nothing");
    }

    /// A missing tip falls back to a round one and says so. Silence here would look like the brush
    /// engine misbehaving rather than a file having moved.
    #[test]
    fn a_missing_tip_is_reported_not_hidden() {
        let asked = TipRef::File {
            path: "nowhere/chalk.png".to_owned(),
        };
        match resolve_tip(&asked, |_| Err("no such file".to_owned())) {
            Resolved::Missing(said) => {
                assert!(said.contains("chalk.png"), "it has to name the tip: {said}");
                assert!(
                    said.contains("round tip"),
                    "and say what it did instead: {said}"
                );
            }
            _ => panic!("a tip that cannot be loaded must not report success"),
        }
        assert!(matches!(
            resolve_tip(&TipRef::Round, |_| Err(String::new())),
            Resolved::Round
        ));
    }
}
