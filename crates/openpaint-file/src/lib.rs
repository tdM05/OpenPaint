//! The `.openpaint` document format.
//!
//! A **SQLite database**, not a zip. That reverses the sketch in `docs/DECISIONS.md` §7, and
//! the reason is the one thing a zip cannot do: replace an entry in place. Any save to a zip
//! rewrites the whole archive, so one stroke in a three-hundred-page sketchbook — the stated
//! core use case (§5) — would cost a full multi-hundred-megabyte rewrite, and autosave would be
//! impossible. Autosave is the feature that actually protects work, so that is disqualifying
//! rather than merely slow.
//!
//! What SQLite buys, none of which is free otherwise:
//!
//! - **Atomicity and crash safety** from transactions, rather than hand-rolled
//!   write-temp-and-rename with its edge cases.
//! - **Incremental saves**: only the tiles that changed are written.
//! - **A self-describing schema** you can dump with any SQLite tool, plus `user_version` for a
//!   well-worn migration story.
//! - A container format with an explicit long-term stability commitment, which is a stronger
//!   longevity guarantee than a convention of ours layered over zip.
//!
//! Clip Studio Paint's `.clip` is also a SQLite database, as far as public reverse-engineering
//! shows. Not a reason on its own, but a signal that the choice survives contact with real
//! documents. Krita and Procreate use zips, and both have the rewrite problem.
//!
//! # This crate has no GPU and no UI
//!
//! It works on `openpaint_core` types and on tile **bytes**, so it is fully testable without a
//! device, and a future tool — a thumbnailer, a CBZ exporter, a CLI — can read a document
//! without pulling in wgpu.
//!
//! # What is and is not saved
//!
//! Saved: the document structure (page rectangles, DPI, mode, the layer stack with names,
//! opacity, blend mode, visibility and **ids**), and every tile of every layer — including the
//! ones lying outside the page, which is what makes a non-destructive crop (§5c) survive being
//! closed and reopened. Without persistence that guarantee only lasted a session.
//!
//! Not saved: undo history. A save is a fresh undo baseline, which is precisely why crop had to
//! be non-destructive rather than "recoverable with Ctrl+Z".

use std::collections::HashMap;
use std::path::Path;

use openpaint_core::tile::{Tile, TileCoord, TILE_BYTES};
use openpaint_core::{Blend, Document, Layer, Mode, Page, PageRect};

/// Schema version this build writes, and the newest it can read.
///
/// Bumped only for changes the *reader* cannot cope with. Adding a tile codec does not need a
/// bump, because the codec is recorded per tile (see [`Codec`]); adding a nullable column does
/// not either, because a reader that does not know it never asks.
pub const SCHEMA_VERSION: i32 = 1;

/// How a tile's bytes are encoded in the file.
///
/// Recorded per tile rather than per document, so a better codec can be introduced without a
/// schema version bump and without rewriting existing files: new tiles get the new codec, old
/// ones keep reading as they always did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    /// Raw `f16` RGBA, linear and premultiplied — byte-identical to what a tile holds in
    /// memory and on the GPU. No conversion on save, so no lossy round trip.
    Raw = 0,
    /// The same bytes, deflated. Layer tiles are mostly untouched (zero) where nothing was
    /// painted, which deflate crushes.
    Deflate = 1,
}

impl Codec {
    fn from_i64(v: i64) -> Option<Self> {
        match v {
            0 => Some(Self::Raw),
            1 => Some(Self::Deflate),
            _ => None,
        }
    }
}

/// Which tile: page position, the layer's **id**, and the tile coordinate.
///
/// Keyed by layer *id*, not by its position in the stack, so reordering layers rewrites no tile
/// rows at all. That is the same reason the in-memory store keys by id, and it is what keeps
/// saves incremental.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TileRef {
    pub page: usize,
    pub layer_id: u32,
    pub coord: TileCoord,
}

#[derive(Debug)]
pub enum Error {
    Db(rusqlite::Error),
    Io(std::io::Error),
    /// The file was written by a newer build than this one.
    TooNew {
        found: i32,
        supported: i32,
    },
    /// The file is a database but not one of ours, or is missing what it promised.
    Malformed(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Db(e) => write!(f, "{e}"),
            Self::Io(e) => write!(f, "{e}"),
            Self::TooNew { found, supported } => write!(
                f,
                "this file was saved by a newer version of OpenPaint \
                 (format {found}; this build reads up to {supported})"
            ),
            Self::Malformed(what) => write!(f, "not a valid OpenPaint document: {what}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Self::Db(e)
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// A document read from a file: its structure, and its tiles by reference.
pub struct Loaded {
    pub document: Document,
    pub tiles: HashMap<TileRef, Tile>,
}

/// Text names for blend modes, not integers.
///
/// A file you can read with `sqlite3` and understand is worth a string compare per layer at load
/// time. Integers would also make a future mode collide with a code some old file already used;
/// `Blend::code()` stays an integer because the *shader* needs one, and that is a different
/// concern with a different lifetime.
fn blend_name(b: Blend) -> &'static str {
    match b {
        Blend::Normal => "normal",
        Blend::Multiply => "multiply",
        Blend::Screen => "screen",
    }
}

fn blend_from_name(s: &str) -> Option<Blend> {
    match s {
        "normal" => Some(Blend::Normal),
        "multiply" => Some(Blend::Multiply),
        "screen" => Some(Blend::Screen),
        _ => None,
    }
}

fn mode_name(m: Mode) -> &'static str {
    match m {
        Mode::Pages => "pages",
        Mode::Continuous => "continuous",
    }
}

fn mode_from_name(s: &str) -> Option<Mode> {
    match s {
        "pages" => Some(Mode::Pages),
        "continuous" => Some(Mode::Continuous),
        _ => None,
    }
}

/// Compress a tile, keeping whichever result is smaller.
///
/// Deflate is a clear win on a partly-painted tile and a small loss on a dense one, so the
/// choice is made per tile from the measured size rather than assumed. This is why [`Codec`] is
/// a column.
fn encode(tile: &Tile) -> (Codec, Vec<u8>) {
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    use std::io::Write;

    let raw = tile.bytes();
    let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
    if enc.write_all(raw).is_ok() {
        if let Ok(packed) = enc.finish() {
            if packed.len() < raw.len() {
                return (Codec::Deflate, packed);
            }
        }
    }
    (Codec::Raw, raw.to_vec())
}

fn decode(codec: Codec, bytes: &[u8]) -> Result<Tile, Error> {
    let raw = match codec {
        Codec::Raw => bytes.to_vec(),
        Codec::Deflate => {
            use flate2::read::ZlibDecoder;
            use std::io::Read;
            let mut out = Vec::with_capacity(TILE_BYTES);
            ZlibDecoder::new(bytes)
                .read_to_end(&mut out)
                .map_err(|e| Error::Malformed(format!("tile did not decompress: {e}")))?;
            out
        }
    };
    Tile::from_bytes(&raw).ok_or_else(|| {
        Error::Malformed(format!(
            "tile is {} bytes, expected {TILE_BYTES}",
            raw.len()
        ))
    })
}

/// Create the schema in an empty database.
fn create_schema(db: &rusqlite::Connection) -> Result<(), Error> {
    db.execute_batch(
        "
        CREATE TABLE document (
            id            INTEGER PRIMARY KEY CHECK (id = 1),
            written_by    TEXT    NOT NULL,
            mode          TEXT    NOT NULL,
            active_page   INTEGER NOT NULL
        );
        CREATE TABLE page (
            idx           INTEGER PRIMARY KEY,
            x             INTEGER NOT NULL,
            y             INTEGER NOT NULL,
            w             INTEGER NOT NULL,
            h             INTEGER NOT NULL,
            dpi           REAL    NOT NULL,
            active_layer  INTEGER NOT NULL,
            next_layer_id INTEGER NOT NULL
        );
        CREATE TABLE layer (
            page_idx INTEGER NOT NULL REFERENCES page(idx) ON DELETE CASCADE,
            idx      INTEGER NOT NULL,
            id       INTEGER NOT NULL,
            name     TEXT    NOT NULL,
            opacity  REAL    NOT NULL,
            blend    TEXT    NOT NULL,
            visible  INTEGER NOT NULL,
            PRIMARY KEY (page_idx, idx)
        );
        CREATE TABLE tile (
            page_idx INTEGER NOT NULL,
            layer_id INTEGER NOT NULL,
            tx       INTEGER NOT NULL,
            ty       INTEGER NOT NULL,
            codec    INTEGER NOT NULL,
            bytes    BLOB    NOT NULL,
            PRIMARY KEY (page_idx, layer_id, tx, ty)
        ) WITHOUT ROWID;
        ",
    )?;
    db.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    Ok(())
}

/// Write a document and its tiles to `path`, creating or replacing it.
///
/// One transaction, so a save that is interrupted leaves the previous contents intact rather
/// than a half-written file. The structure tables are rewritten wholesale — they are kilobytes —
/// while tiles are replaced by key, so a save costs the tiles that changed plus a little.
pub fn save(
    path: &Path,
    document: &Document,
    tiles: impl IntoIterator<Item = (TileRef, Tile)>,
) -> Result<(), Error> {
    let mut db = rusqlite::Connection::open(path)?;
    // WAL so an interrupted write cannot corrupt the file, and NORMAL because the transaction
    // boundary is what we rely on for consistency, not a flush per statement.
    db.pragma_update(None, "journal_mode", "WAL")?;
    db.pragma_update(None, "synchronous", "NORMAL")?;

    let existing: i32 = db.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if existing == 0 {
        create_schema(&db)?;
    } else if existing > SCHEMA_VERSION {
        return Err(Error::TooNew {
            found: existing,
            supported: SCHEMA_VERSION,
        });
    }

    let tx = db.transaction()?;
    tx.execute("DELETE FROM layer", [])?;
    tx.execute("DELETE FROM page", [])?;
    tx.execute("DELETE FROM document", [])?;
    tx.execute(
        "INSERT INTO document (id, written_by, mode, active_page) VALUES (1, ?1, ?2, ?3)",
        rusqlite::params![
            concat!("openpaint ", env!("CARGO_PKG_VERSION")),
            mode_name(document.mode()),
            document.active_index() as i64,
        ],
    )?;

    for index in 0..document.page_count() {
        let page = document
            .page(index)
            .ok_or_else(|| Error::Malformed(format!("page {index} vanished while saving")))?;
        let r = page.rect();
        tx.execute(
            "INSERT INTO page (idx, x, y, w, h, dpi, active_layer, next_layer_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                index as i64,
                r.x,
                r.y,
                r.w,
                r.h,
                page.dpi(),
                page.active_index() as i64,
                i64::from(page.next_layer_id()),
            ],
        )?;
        for (li, layer) in page.layers().iter().enumerate() {
            tx.execute(
                "INSERT INTO layer (page_idx, idx, id, name, opacity, blend, visible)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![
                    index as i64,
                    li as i64,
                    i64::from(layer.id()),
                    layer.name,
                    layer.opacity,
                    blend_name(layer.blend),
                    i64::from(layer.visible),
                ],
            )?;
        }
    }

    {
        let mut insert = tx.prepare(
            "INSERT OR REPLACE INTO tile (page_idx, layer_id, tx, ty, codec, bytes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for (key, tile) in tiles {
            let (codec, bytes) = encode(&tile);
            insert.execute(rusqlite::params![
                key.page as i64,
                i64::from(key.layer_id),
                key.coord.0,
                key.coord.1,
                codec as i64,
                bytes,
            ])?;
        }
    }

    tx.commit()?;
    Ok(())
}

/// Read a document and its tiles from `path`.
pub fn load(path: &Path) -> Result<Loaded, Error> {
    let db = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )?;

    let version: i32 = db.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if version == 0 {
        return Err(Error::Malformed("no format version".into()));
    }
    if version > SCHEMA_VERSION {
        return Err(Error::TooNew {
            found: version,
            supported: SCHEMA_VERSION,
        });
    }

    let (mode_text, active_page): (String, i64) = db
        .query_row(
            "SELECT mode, active_page FROM document WHERE id = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|_| Error::Malformed("missing document row".into()))?;
    let mode = mode_from_name(&mode_text)
        .ok_or_else(|| Error::Malformed(format!("unknown mode {mode_text:?}")))?;

    let mut pages = Vec::new();
    let mut page_stmt = db.prepare(
        "SELECT idx, x, y, w, h, dpi, active_layer, next_layer_id FROM page ORDER BY idx",
    )?;
    let mut layer_stmt = db.prepare(
        "SELECT id, name, opacity, blend, visible FROM layer WHERE page_idx = ?1 ORDER BY idx",
    )?;

    let rows = page_stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            PageRect::new(r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?),
            r.get::<_, f64>(5)? as f32,
            r.get::<_, i64>(6)?,
            r.get::<_, i64>(7)?,
        ))
    })?;

    for row in rows {
        let (idx, rect, dpi, active_layer, next_layer_id) = row?;
        let layers: Vec<Layer> = layer_stmt
            .query_map([idx], |r| {
                let blend_text: String = r.get(3)?;
                Ok((
                    r.get::<_, i64>(0)? as u32,
                    r.get::<_, String>(1)?,
                    r.get::<_, f64>(2)? as f32,
                    blend_text,
                    r.get::<_, i64>(4)? != 0,
                ))
            })?
            .map(|row| {
                let (id, name, opacity, blend_text, visible) = row?;
                let blend = blend_from_name(&blend_text)
                    .ok_or_else(|| Error::Malformed(format!("unknown blend {blend_text:?}")))?;
                Ok(Layer::restored(id, name, opacity, blend, visible))
            })
            .collect::<Result<_, Error>>()?;

        let page = Page::restored(
            rect,
            dpi,
            layers,
            usize::try_from(active_layer).unwrap_or(0),
            u32::try_from(next_layer_id).unwrap_or(0),
        )
        .ok_or_else(|| Error::Malformed(format!("page {idx} has no layers")))?;
        pages.push(page);
    }

    let document = Document::restored(pages, usize::try_from(active_page).unwrap_or(0), mode)
        .ok_or_else(|| Error::Malformed("document has no pages".into()))?;

    let mut tiles = HashMap::new();
    let mut tile_stmt = db.prepare("SELECT page_idx, layer_id, tx, ty, codec, bytes FROM tile")?;
    let rows = tile_stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, i32>(2)?,
            r.get::<_, i32>(3)?,
            r.get::<_, i64>(4)?,
            r.get::<_, Vec<u8>>(5)?,
        ))
    })?;
    for row in rows {
        let (page, layer_id, tx, ty, codec, bytes) = row?;
        let codec = Codec::from_i64(codec)
            .ok_or_else(|| Error::Malformed(format!("unknown tile codec {codec}")))?;
        tiles.insert(
            TileRef {
                page: usize::try_from(page).unwrap_or(0),
                layer_id: u32::try_from(layer_id).unwrap_or(0),
                coord: (tx, ty),
            },
            decode(codec, &bytes)?,
        );
    }

    Ok(Loaded { document, tiles })
}

#[cfg(test)]
mod tests;
