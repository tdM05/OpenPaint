# Example brush tips

A brush tip is a PNG of the mark a single dab makes. It replaces the procedural
edge: where a round tip computes coverage from distance, a stamped one reads it
out of the image (see `docs/DECISIONS.md` §4p).

**What looks like ink is ink.** Two conventions are common and both work:

- `chalk.png` — drawn on transparent. Read from the alpha channel. This is what
  falls out of exporting from any paint app.
- `flat-nib.png` — drawn in black on white, fully opaque. Read from inverted
  luminance. This is how Photoshop's own `.abr` tips are made.
- `square-outline.png` — a hard border, for checking the mapping by eye. The
  outline should sit exactly on the dab's own square, so a stroke of it reads
  as two parallel lines with a hollow middle.

These are generated examples, not artwork. Load one with **Load brush tip…** in
the Brush section.

A tip is an *app resource*, not document content — a tool you own, like a font,
rather than part of the artwork. Nothing about it is written into a `.openpaint`
file, which is what keeps a document openable on a machine that does not have
the tip.
