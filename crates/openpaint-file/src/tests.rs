//! Round-trip tests for the document format.
//!
//! No GPU and no UI here, which is the point of this crate being separate: the format is
//! testable on its own, and the tests can be exhaustive about structure rather than sampling
//! pixels through a device.

use super::*;

/// A temporary file path that removes itself, so a failing test does not leave litter or make
/// the next run pass on stale data.
struct TempFile(std::path::PathBuf);

impl TempFile {
    fn new(tag: &str) -> Self {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        Self(std::env::temp_dir().join(format!("openpaint-test-{tag}-{stamp}.openpaint")))
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        // WAL leaves companions behind; a stale one would be read on the next open.
        for suffix in ["-wal", "-shm"] {
            let mut p = self.0.clone().into_os_string();
            p.push(suffix);
            let _ = std::fs::remove_file(std::path::PathBuf::from(p));
        }
    }
}

/// A document with two pages, several layers, and every blend mode represented.
fn sample() -> Document {
    let mut doc = Document::new(Page::new(1000, 1400));
    doc.add_layer();
    doc.add_layer();
    {
        let first = doc.active_mut();
        first.layer_mut(0).expect("bottom").name = "Background".into();
        first.layer_mut(1).expect("mid").blend = Blend::Multiply;
        first.layer_mut(1).expect("mid").opacity = 0.42;
        first.layer_mut(1).expect("mid").name = "Shadows".into();
        first.layer_mut(2).expect("top").blend = Blend::Screen;
        first.layer_mut(2).expect("top").visible = false;
        first.set_active(1);
        first.set_dpi(300.0);
    }

    // A second page, extended up and left, so a negative origin is part of the round trip.
    doc.add_page_like_active();
    doc.active_mut()
        .resize(PageRect::new(-300, -120, 1100, 920));
    doc.set_active(1);
    doc
}

/// A tile whose every texel differs, so a byte-level mistake cannot hide behind uniformity.
fn patterned(seed: u32) -> Tile {
    let mut t = Tile::transparent();
    for y in 0..openpaint_core::tile::TILE_SIZE {
        for x in 0..openpaint_core::tile::TILE_SIZE {
            let v = ((x as u32 * 7 + y as u32 * 13 + seed) % 251) as f32 / 251.0;
            // Premultiplied: colour channels never exceed alpha.
            t.set_texel(x, y, [v * 0.5, v * 0.25, v, v]);
        }
    }
    t
}

/// A palette survives a save and a load, in order.
///
/// Stored as authored sRGB rather than converted: a swatch is a colour the artist *chose*, and a
/// round trip through linear at eight bits would drift it a shade every time the file was opened.
/// So the assertion is on the exact bytes, which is the property that matters.
#[test]
fn a_palette_round_trips() {
    let f = TempFile::new("palette");
    let mut doc = sample();
    for rgb in [[0, 0, 0], [255, 231, 200], [12, 34, 56]] {
        assert!(doc.add_to_palette(rgb));
    }

    save(f.path(), &doc, [], &[]).expect("save");
    let back = load(f.path()).expect("load");
    assert_eq!(
        back.document.palette(),
        [[0, 0, 0], [255, 231, 200], [12, 34, 56]]
    );
}

/// A file written before palettes existed loads with none, rather than failing.
///
/// The tolerance every added table has followed since `meta`: absence reads as the default, so
/// reading needs no branch on the schema version. Simulated by dropping the table, which is exactly
/// the shape an older file has.
#[test]
fn a_file_without_a_palette_still_loads() {
    let f = TempFile::new("no-palette");
    let doc = sample();
    save(f.path(), &doc, [], &[]).expect("save");

    let db = rusqlite::Connection::open(f.path()).expect("open");
    db.execute_batch("DROP TABLE palette;").expect("drop");
    drop(db);

    let back = load(f.path()).expect("a document without a palette is not malformed");
    assert!(back.document.palette().is_empty());
}

#[test]
fn a_document_round_trips_exactly() {
    let f = TempFile::new("structure");
    let doc = sample();
    save(f.path(), &doc, [], &[]).expect("save");
    let back = load(f.path()).expect("load");

    assert_eq!(back.document.page_count(), doc.page_count());
    assert_eq!(back.document.active_index(), doc.active_index());

    for i in 0..doc.page_count() {
        let a = doc.page(i).expect("page");
        let b = back.document.page(i).expect("page");
        assert_eq!(a.rect(), b.rect(), "page {i} rectangle");
        assert!((a.dpi() - b.dpi()).abs() < 1e-3, "page {i} dpi");
        assert_eq!(a.active_index(), b.active_index(), "page {i} active layer");
        assert_eq!(a.layer_count(), b.layer_count(), "page {i} layer count");
        for (x, y) in a.layers().iter().zip(b.layers()) {
            assert_eq!(x, y, "layer {:?} did not survive", x.name);
        }
    }
}

/// Layer ids are what tiles are keyed by, so a load that renumbered them would separate every
/// layer from its pixels. This is the single most important thing about the structure.
///
/// Ids are unique across the whole **document**, not per page: the renderer keys tiles by id
/// alone, so two pages both starting at 0 would have their pixels collide.
#[test]
fn layer_ids_survive_and_the_counter_stays_ahead() {
    let f = TempFile::new("ids");
    let mut doc = sample();
    // Page 0 is the one with a stack; delete a middle layer so there is a gap in the ids, which
    // a counter derived from the layer count would then hand out again.
    let removed = doc
        .page_mut(0)
        .expect("page")
        .remove_layer(1)
        .expect("removable");
    save(f.path(), &doc, [], &[]).expect("save");
    let mut back = load(f.path()).expect("load");

    let ids: Vec<u32> = doc
        .page(0)
        .expect("page")
        .layers()
        .iter()
        .map(Layer::id)
        .collect();
    let back_ids: Vec<u32> = back
        .document
        .page(0)
        .expect("page")
        .layers()
        .iter()
        .map(Layer::id)
        .collect();
    assert_eq!(ids, back_ids, "ids changed across a save");

    back.document.set_active(0);
    back.document.add_layer();
    let new_id = back.document.active().active_layer().id();
    assert!(
        !ids.contains(&new_id) && new_id != removed,
        "a new layer reused id {new_id}; ids were {ids:?} and {removed} was deleted"
    );
}

/// Pixels must come back bit-for-bit. They are stored as the same premultiplied `f16` bytes the
/// GPU holds, so anything other than exact equality means a conversion crept in.
#[test]
fn tiles_round_trip_bit_for_bit() {
    let f = TempFile::new("tiles");
    let doc = sample();
    let refs = [
        TileRef {
            page: 0,
            layer_id: 0,
            coord: (0, 0),
        },
        TileRef {
            page: 0,
            layer_id: 2,
            coord: (3, 5),
        },
        // Negative coordinates are the normal case on a page extended up or left.
        TileRef {
            page: 1,
            layer_id: 0,
            coord: (-2, -1),
        },
    ];
    let originals: Vec<Tile> = (0..refs.len()).map(|i| patterned(i as u32 * 31)).collect();

    save(
        f.path(),
        &doc,
        refs.iter().copied().zip(originals.iter().cloned()),
        &[],
    )
    .expect("save");
    let back = load(f.path()).expect("load");

    assert_eq!(back.tiles.len(), refs.len(), "wrong number of tiles");
    for (key, original) in refs.iter().zip(&originals) {
        let got = back
            .tiles
            .get(key)
            .unwrap_or_else(|| panic!("missing {key:?}"));
        assert_eq!(got.bytes(), original.bytes(), "{key:?} came back different");
    }
}

/// A tile outside the page must be saved too. That is what makes a non-destructive crop
/// (DECISIONS §5c) survive being closed and reopened -- without it the guarantee lasted only as
/// long as the session.
#[test]
fn tiles_outside_the_page_are_saved() {
    let f = TempFile::new("outside");
    let mut doc = sample();
    doc.active_mut().resize(PageRect::from_size(300, 300));
    // Tile (5, 5) starts at pixel 1280, well outside a 300x300 page.
    let far = TileRef {
        page: 1,
        layer_id: 0,
        coord: (5, 5),
    };

    save(f.path(), &doc, [(far, patterned(9))], &[]).expect("save");
    let back = load(f.path()).expect("load");
    assert!(
        back.tiles.contains_key(&far),
        "the cropped-away tile was dropped by the format"
    );
}

/// Deflate must actually be chosen where it helps, or saves are needlessly large -- a
/// mostly-untouched tile is the common case for line art.
#[test]
fn a_sparse_tile_compresses() {
    let mut t = Tile::transparent();
    for x in 0..32 {
        t.set_texel(x, 0, [0.0, 0.0, 0.0, 1.0]);
    }
    let (codec, bytes) = encode(&t);
    assert_eq!(codec, Codec::Deflate);
    assert!(
        bytes.len() * 20 < TILE_BYTES,
        "a nearly-empty tile compressed to {} of {TILE_BYTES} bytes",
        bytes.len()
    );
    assert_eq!(decode(codec, &bytes).expect("decode").bytes(), t.bytes());
}

/// Whichever codec is picked, the bytes must survive. Raw is the fallback for data deflate
/// cannot shrink, and it has to be exercised rather than assumed.
#[test]
fn both_codecs_round_trip() {
    let t = patterned(5);
    for codec in [Codec::Raw, Codec::Deflate] {
        let bytes = match codec {
            Codec::Raw => t.bytes().to_vec(),
            Codec::Deflate => {
                let (c, b) = encode(&t);
                assert_eq!(c, Codec::Deflate, "the patterned tile should compress");
                b
            }
        };
        assert_eq!(
            decode(codec, &bytes).expect("decode").bytes(),
            t.bytes(),
            "{codec:?} did not round trip"
        );
    }
}

/// Saving twice must leave one document, not two stacked on top of each other. The structure
/// tables are rewritten wholesale, and forgetting to clear one of them is the obvious way to
/// end up with a file that loads as gibberish.
#[test]
fn saving_twice_replaces_rather_than_accumulates() {
    let f = TempFile::new("resave");
    let doc = sample();
    let key = TileRef {
        page: 0,
        layer_id: 0,
        coord: (1, 1),
    };
    save(f.path(), &doc, [(key, patterned(1))], &[]).expect("first save");

    // A smaller document over the top of a larger one: the extra page must not linger.
    let doc2 = Document::new(Page::new(200, 200));
    save(f.path(), &doc2, [(key, patterned(2))], &[]).expect("second save");

    let back = load(f.path()).expect("load");
    assert_eq!(back.document.page_count(), 1, "the old page survived");
    assert_eq!(back.tiles.len(), 1);
    assert_eq!(
        back.tiles.get(&key).expect("tile").bytes(),
        patterned(2).bytes(),
        "the tile was not replaced"
    );
}

/// A file from a newer build must be refused with an explanation, not misread. Silently ignoring
/// a version we do not understand is how a format corrupts documents.
#[test]
fn a_newer_file_is_refused() {
    let f = TempFile::new("newer");
    save(f.path(), &sample(), [], &[]).expect("save");
    {
        let db = rusqlite::Connection::open(f.path()).expect("open");
        db.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .expect("bump");
    }
    match load(f.path()) {
        Err(Error::TooNew { found, supported }) => {
            assert_eq!(found, SCHEMA_VERSION + 1);
            assert_eq!(supported, SCHEMA_VERSION);
        }
        Err(other) => panic!("expected a TooNew refusal, got {other:?}"),
        Ok(_) => panic!("a newer file was loaded instead of refused"),
    }
    // And saving into it must refuse too, rather than writing our schema over theirs.
    assert!(matches!(
        save(f.path(), &sample(), [], &[]),
        Err(Error::TooNew { .. })
    ));
}

/// A version-1 file must still open, and saving over it must bring it up to date.
///
/// The first real schema change, and therefore the first chance to prove the migration story
/// rather than assert it. Version 2 dropped `document.mode` and `page.next_layer_id`, both
/// `NOT NULL`, so a version-1 file cannot be written to without migrating -- and reading must
/// need no branch at all, since the reader stops asking for what it no longer wants.
#[test]
fn a_version_one_file_migrates() {
    let f = TempFile::new("v1");

    // Build a version-1 file by hand: the old schema, with the two dead columns.
    {
        let db = rusqlite::Connection::open(f.path()).expect("open");
        db.execute_batch(
            "CREATE TABLE document (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                written_by TEXT NOT NULL,
                mode TEXT NOT NULL,
                active_page INTEGER NOT NULL);
             CREATE TABLE page (
                idx INTEGER PRIMARY KEY, x INTEGER NOT NULL, y INTEGER NOT NULL,
                w INTEGER NOT NULL, h INTEGER NOT NULL, dpi REAL NOT NULL,
                active_layer INTEGER NOT NULL, next_layer_id INTEGER NOT NULL);
             CREATE TABLE layer (
                page_idx INTEGER NOT NULL, idx INTEGER NOT NULL, id INTEGER NOT NULL,
                name TEXT NOT NULL, opacity REAL NOT NULL, blend TEXT NOT NULL,
                visible INTEGER NOT NULL, PRIMARY KEY (page_idx, idx));
             CREATE TABLE tile (
                page_idx INTEGER NOT NULL, layer_id INTEGER NOT NULL,
                tx INTEGER NOT NULL, ty INTEGER NOT NULL, codec INTEGER NOT NULL,
                bytes BLOB NOT NULL, PRIMARY KEY (page_idx, layer_id, tx, ty)) WITHOUT ROWID;
             INSERT INTO document VALUES (1, 'openpaint 0.0.0', 'continuous', 0);
             INSERT INTO page VALUES (0, -10, -20, 700, 900, 300.0, 1, 9);
             INSERT INTO layer VALUES (0, 0, 3, 'Ink', 1.0, 'normal', 1);
             INSERT INTO layer VALUES (0, 1, 7, 'Shade', 0.5, 'multiply', 0);",
        )
        .expect("v1 schema");
        let (codec, bytes) = encode(&patterned(4));
        db.execute(
            "INSERT INTO tile VALUES (0, 7, 2, -3, ?1, ?2)",
            rusqlite::params![codec as i64, bytes],
        )
        .expect("v1 tile");
        db.pragma_update(None, "user_version", 1).expect("version");
    }

    // Reading it needs no migration: everything version 2 asks for is already there.
    let back = load(f.path()).expect("a version-1 file should still load");
    let page = back.document.active();
    assert_eq!(page.rect(), PageRect::new(-10, -20, 700, 900));
    assert!((page.dpi() - 300.0).abs() < 1e-3);
    assert_eq!(page.active_index(), 1);
    let ids: Vec<u32> = page.layers().iter().map(Layer::id).collect();
    assert_eq!(ids, vec![3, 7], "layer ids did not survive the old schema");
    assert_eq!(page.layers()[1].blend, Blend::Multiply);
    assert!(!page.layers()[1].visible);

    let key = TileRef {
        page: 0,
        layer_id: 7,
        coord: (2, -3),
    };
    assert_eq!(
        back.tiles.get(&key).expect("the v1 tile").bytes(),
        patterned(4).bytes()
    );

    // Saving migrates the file, keeps the tile, and leaves it at the current version.
    save(f.path(), &back.document, [], &[]).expect("save over a v1 file");
    {
        let db = rusqlite::Connection::open(f.path()).expect("reopen");
        let version: i32 = db
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .expect("version");
        assert_eq!(version, SCHEMA_VERSION, "the file was not migrated");
        // The dead columns are gone.
        assert!(
            db.prepare("SELECT mode FROM document").is_err(),
            "document.mode survived the migration"
        );
        assert!(
            db.prepare("SELECT next_layer_id FROM page").is_err(),
            "page.next_layer_id survived the migration"
        );
    }

    // And the tile the migration did not touch is still readable.
    let again = load(f.path()).expect("load after migrating");
    assert_eq!(
        again.tiles.get(&key).expect("tile after migration").bytes(),
        patterned(4).bytes(),
        "the migration lost a tile"
    );
    let ids: Vec<u32> = again
        .document
        .active()
        .layers()
        .iter()
        .map(Layer::id)
        .collect();
    assert_eq!(ids, vec![3, 7]);
}

/// Something that is not one of our documents must fail as data, not panic.
#[test]
fn a_foreign_file_is_rejected() {
    let f = TempFile::new("foreign");
    std::fs::write(f.path(), b"this is not a database at all").expect("write");
    assert!(load(f.path()).is_err());

    // A valid SQLite database that is not ours.
    let g = TempFile::new("otherdb");
    {
        let db = rusqlite::Connection::open(g.path()).expect("open");
        db.execute_batch("CREATE TABLE unrelated (a INTEGER);")
            .expect("schema");
    }
    match load(g.path()) {
        Err(Error::Malformed(_)) => {}
        Err(other) => panic!("expected Malformed, got {other:?}"),
        Ok(_) => panic!("a foreign database was loaded as a document"),
    }
}

/// Blend modes are stored by name, so an unknown one has to be reported rather than silently
/// becoming Normal -- which would quietly change how a document looks.
#[test]
fn an_unknown_blend_mode_is_reported() {
    let f = TempFile::new("blend");
    save(f.path(), &sample(), [], &[]).expect("save");
    {
        let db = rusqlite::Connection::open(f.path()).expect("open");
        db.execute("UPDATE layer SET blend = 'hard-light' WHERE idx = 1", [])
            .expect("tamper");
    }
    match load(f.path()) {
        Err(Error::Malformed(why)) => assert!(why.contains("hard-light"), "{why}"),
        Err(other) => panic!("expected Malformed, got {other:?}"),
        Ok(_) => panic!("an unknown blend mode loaded silently"),
    }
}

/// Two pages must keep their pixels apart. Tiles are keyed by layer id alone, so this is the
/// property that made layer ids document-wide -- with per-page ids both pages' first layers would
/// be id 0 and their tiles would land on top of each other.
#[test]
fn two_pages_keep_their_tiles_apart() {
    let f = TempFile::new("pages");
    let mut doc = Document::new(Page::new(400, 400));
    let first_layer = doc.active().active_layer().id();
    doc.add_page_like_active();
    let second_layer = doc.active().active_layer().id();
    assert_ne!(first_layer, second_layer, "pages shared a layer id");

    // The same tile coordinate on each page, with different pixels.
    let a = TileRef {
        page: 0,
        layer_id: first_layer,
        coord: (0, 0),
    };
    let b = TileRef {
        page: 1,
        layer_id: second_layer,
        coord: (0, 0),
    };
    save(f.path(), &doc, [(a, patterned(1)), (b, patterned(2))], &[]).expect("save");

    let back = load(f.path()).expect("load");
    assert_eq!(back.document.page_count(), 2);
    assert_eq!(back.tiles.len(), 2, "one page's tile overwrote the other's");
    assert_eq!(
        back.tiles.get(&a).expect("page 0 tile").bytes(),
        patterned(1).bytes()
    );
    assert_eq!(
        back.tiles.get(&b).expect("page 1 tile").bytes(),
        patterned(2).bytes()
    );
}

/// A page's own geometry has to survive independently -- a sketchbook of differently-sized pages
/// is an ordinary document, not an edge case.
#[test]
fn pages_keep_their_own_geometry() {
    let f = TempFile::new("geometry");
    let mut doc = Document::new(Page::new(400, 400));
    doc.add_page_like_active();
    doc.active_mut().resize(PageRect::new(-50, -60, 900, 1500));
    doc.active_mut().set_dpi(600.0);
    doc.set_active(0);

    save(f.path(), &doc, [], &[]).expect("save");
    let back = load(f.path()).expect("load");
    assert_eq!(
        back.document.page(0).expect("page").rect(),
        PageRect::from_size(400, 400)
    );
    assert_eq!(
        back.document.page(1).expect("page").rect(),
        PageRect::new(-50, -60, 900, 1500)
    );
    assert!((back.document.page(1).expect("page").dpi() - 600.0).abs() < 1e-3);
    assert_eq!(back.document.active_index(), 0, "the active page changed");
}

/// Every blend mode must have a name and get back to itself, or a mode added later can be saved
/// and then fail to load.
#[test]
fn every_blend_mode_has_a_stable_name() {
    for mode in Blend::ALL {
        let name = blend_name(mode);
        assert_eq!(
            blend_from_name(name),
            Some(mode),
            "{name} did not round trip"
        );
    }
}

/// Metadata survives a round trip, because autosave's recovery depends on it.
#[test]
fn metadata_round_trips() {
    let f = TempFile::new("meta");
    save(
        f.path(),
        &sample(),
        [],
        &[
            ("autosave.recovery", "1"),
            ("autosave.original_path", r"C:\art\page one.openpaint"),
        ],
    )
    .expect("save");

    let back = load(f.path()).expect("load");
    assert_eq!(
        back.meta.get("autosave.recovery").map(String::as_str),
        Some("1")
    );
    assert_eq!(
        back.meta.get("autosave.original_path").map(String::as_str),
        Some(r"C:\art\page one.openpaint"),
        "a path with spaces and backslashes has to survive verbatim"
    );

    // And the cheap reader agrees with the full load, since autosave uses it at startup instead of
    // paying for every tile.
    let only_meta = read_meta(f.path()).expect("read_meta");
    assert_eq!(only_meta, back.meta);
}

/// A later save must replace metadata, not accumulate it.
///
/// Otherwise a document saved normally would keep the `autosave.recovery` marker it picked up from
/// having once been a recovery copy, and every launch would offer to recover a file that is safe.
#[test]
fn metadata_is_replaced_not_merged() {
    let f = TempFile::new("meta-replace");
    save(f.path(), &sample(), [], &[("autosave.recovery", "1")]).expect("first save");
    save(f.path(), &sample(), [], &[]).expect("second save");

    let back = load(f.path()).expect("load");
    assert!(
        back.meta.is_empty(),
        "stale metadata survived a save that specified none: {:?}",
        back.meta
    );
}

/// A file with no `meta` table at all reads as having no metadata.
///
/// This is the pre-v3 case, and the reason `load` tolerates the table's absence rather than
/// branching on the schema version: absence reads as absence.
#[test]
fn a_file_without_the_meta_table_still_loads() {
    let f = TempFile::new("meta-absent");
    save(f.path(), &sample(), [], &[("gone", "soon")]).expect("save");
    {
        let db = rusqlite::Connection::open(f.path()).expect("reopen");
        db.execute_batch("DROP TABLE meta").expect("drop meta");
        // Pretend it was written by the version that had no such table.
        db.pragma_update(None, "user_version", 2)
            .expect("downgrade");
    }

    let back = load(f.path()).expect("a v2 file must still load");
    assert!(back.meta.is_empty());
    assert!(read_meta(f.path()).expect("read_meta").is_empty());

    // And saving brings the table back rather than failing on its absence.
    save(f.path(), &back.document, [], &[("back", "again")]).expect("save migrates");
    assert_eq!(
        load(f.path())
            .expect("load")
            .meta
            .get("back")
            .map(String::as_str),
        Some("again")
    );
}

/// Alpha lock is a document property, so it has to survive a round trip.
///
/// And a pre-v4 file must still load: the column is new, and reading falls back to supplying its
/// default in SQL rather than branching on the schema version.
#[test]
fn alpha_lock_round_trips_and_older_files_default_it_off() {
    let f = TempFile::new("lock-alpha");
    let mut doc = sample();
    // Two layers, so the test can tell "locked" from "every layer came back locked".
    doc.add_layer();
    doc.active_mut().layer_mut(0).expect("layer").lock_alpha = true;
    save(f.path(), &doc, [], &[]).expect("save");

    let back = load(f.path()).expect("load");
    assert!(
        back.document.active().layers()[0].lock_alpha,
        "the lock did not survive the file"
    );
    assert!(
        !back.document.active().layers()[1].lock_alpha,
        "an unlocked layer must not come back locked"
    );

    // Now make it look like a v3 file, which had no such column at all.
    {
        let db = rusqlite::Connection::open(f.path()).expect("reopen");
        db.execute_batch(
            "CREATE TABLE layer_old AS SELECT page_idx, idx, id, name, opacity, blend, visible
             FROM layer;
             DROP TABLE layer;
             ALTER TABLE layer_old RENAME TO layer;",
        )
        .expect("drop the column");
        db.pragma_update(None, "user_version", 3)
            .expect("downgrade");
    }

    let old = load(f.path()).expect("a v3 file must still load");
    assert!(
        old.document.active().layers().iter().all(|l| !l.lock_alpha),
        "a file written before the column existed should read as unlocked"
    );

    // And saving migrates it forward, column and all.
    save(f.path(), &old.document, [], &[]).expect("save migrates");
    let db = rusqlite::Connection::open(f.path()).expect("reopen");
    let version: i32 = db
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .expect("version");
    assert_eq!(version, SCHEMA_VERSION);
}

/// Clipping is a document property too, and older files default it off.
#[test]
fn clip_below_round_trips() {
    let f = TempFile::new("clip-below");
    let mut doc = sample();
    doc.add_layer();
    doc.active_mut().layer_mut(1).expect("top").clip_below = true;
    save(f.path(), &doc, [], &[]).expect("save");

    let back = load(f.path()).expect("load");
    let layers = back.document.active().layers();
    assert!(
        layers[1].clip_below,
        "the clip flag did not survive the file"
    );
    assert!(
        !layers[0].clip_below,
        "an unclipped layer must not come back clipped"
    );
    // The two flags are independent, and adjacent columns are exactly where a mix-up would hide.
    assert!(!layers[1].lock_alpha, "clip_below leaked into lock_alpha");
}

/// A locked layer comes back locked, and nothing else comes back locked with it.
///
/// **A lock that did not survive the save would be worse than no lock at all**: an artist who
/// locked the sketch, saved, reopened and inked would find the lock gone and would have no reason
/// to check. Three adjacent flag columns is exactly where a mix-up hides, so each is asserted
/// against the other two.
#[test]
fn a_locked_layer_round_trips() {
    let f = TempFile::new("locked");
    let mut doc = sample();
    doc.add_layer();
    doc.active_mut().layer_mut(0).expect("bottom").locked = true;
    save(f.path(), &doc, [], &[]).expect("save");

    let back = load(f.path()).expect("load");
    let layers = back.document.active().layers();
    assert!(layers[0].locked, "the lock did not survive the file");
    assert!(!layers[1].locked, "an unlocked layer came back locked");
    assert!(!layers[0].lock_alpha, "locked leaked into lock_alpha");
    assert!(!layers[0].clip_below, "locked leaked into clip_below");
    assert!(layers[0].visible, "locking a layer hid it");
}

/// A caption is only editable on Thursday if the *text* survived the save, not the pixels.
#[test]
fn a_text_layer_round_trips_everything_it_holds() {
    let f = TempFile::new("text");
    let mut doc = sample();
    // Deliberately not a single default: every field has to be checked, because a field left out
    // of the INSERT silently resets on load and looks like a rendering bug much later.
    let block = TextBlock {
        text: "WHAT WAS\nTHAT NOISE?".into(),
        font: FontSpec {
            family: "Comic Sans MS".into(),
            weight: 700,
            italic: true,
        },
        size: 41.5,
        line_height: 1.35,
        letter_spacing: -1.25,
        color_srgb8: [200, 30, 90],
        x: -12.5,
        y: 340.75,
        wrap_width: Some(275.0),
        align: Align::Center,
        writing_mode: WritingMode::Horizontal,
    };
    doc.add_text_layer(block.clone());
    save(f.path(), &doc, [], &[]).expect("save");

    let back = load(f.path()).expect("load");
    let restored = back
        .document
        .active()
        .layers()
        .iter()
        .find_map(openpaint_core::Layer::text)
        .expect("the text layer came back as raster");
    assert_eq!(*restored, block, "a field was lost or altered by the file");
}

/// The layer stack is order-sensitive, and text is attached by index. A block landing on the wrong
/// layer would be invisible in a one-layer test.
#[test]
fn text_stays_attached_to_its_own_layer() {
    let f = TempFile::new("text-index");
    let mut doc = Document::new(Page::new(400, 400));
    doc.add_layer();
    doc.add_text_layer(TextBlock {
        text: "middle".into(),
        ..TextBlock::default()
    });
    doc.add_layer();
    let expected: Vec<Option<String>> = doc
        .active()
        .layers()
        .iter()
        .map(|l| l.text().map(|b| b.text.clone()))
        .collect();
    save(f.path(), &doc, [], &[]).expect("save");

    let back = load(f.path()).expect("load");
    let actual: Vec<Option<String>> = back
        .document
        .active()
        .layers()
        .iter()
        .map(|l| l.text().map(|b| b.text.clone()))
        .collect();
    assert_eq!(actual, expected, "text moved to a different layer");
    assert_eq!(
        actual.iter().filter(|t| t.is_some()).count(),
        1,
        "exactly one layer should hold text"
    );
}

/// Two text layers on one page, so a query that returns the first row for every layer is caught.
#[test]
fn several_text_layers_keep_their_own_words() {
    let f = TempFile::new("text-many");
    let mut doc = Document::new(Page::new(400, 400));
    for word in ["first", "second", "third"] {
        doc.add_text_layer(TextBlock {
            text: word.into(),
            ..TextBlock::default()
        });
    }
    save(f.path(), &doc, [], &[]).expect("save");

    let back = load(f.path()).expect("load");
    let words: Vec<String> = back
        .document
        .active()
        .layers()
        .iter()
        .filter_map(|l| l.text().map(|b| b.text.clone()))
        .collect();
    assert_eq!(words, ["first", "second", "third"]);
}

/// Text on a page other than the first. The block table is keyed by page as well as layer, and a
/// query that forgot the page filter would put page two's captions on page one.
#[test]
fn text_stays_on_its_own_page() {
    let f = TempFile::new("text-pages");
    let mut doc = Document::new(Page::new(400, 400));
    doc.add_page(Page::new(400, 400));
    doc.add_text_layer(TextBlock {
        text: "page two".into(),
        ..TextBlock::default()
    });
    let second = doc.active_index();
    assert!(second > 0, "the new page should be the active one");
    save(f.path(), &doc, [], &[]).expect("save");

    let back = load(f.path()).expect("load");
    assert!(
        back.document
            .page(0)
            .expect("page one")
            .layers()
            .iter()
            .all(|l| l.text().is_none()),
        "page one picked up page two's caption"
    );
    assert_eq!(
        back.document
            .page(second)
            .expect("page two")
            .layers()
            .iter()
            .find_map(openpaint_core::Layer::text)
            .expect("page two lost its caption")
            .text,
        "page two"
    );
}

/// A single line and a wrapped box are the same kind of block with and without a width, so the
/// difference between `None` and `Some` has to survive as itself rather than as a zero.
#[test]
fn an_unwrapped_block_comes_back_unwrapped() {
    let f = TempFile::new("text-wrap");
    let mut doc = Document::new(Page::new(400, 400));
    doc.add_text_layer(TextBlock {
        text: "one line".into(),
        wrap_width: None,
        ..TextBlock::default()
    });
    save(f.path(), &doc, [], &[]).expect("save");

    let back = load(f.path()).expect("load");
    assert_eq!(
        back.document
            .active()
            .layers()
            .iter()
            .find_map(openpaint_core::Layer::text)
            .expect("text layer")
            .wrap_width,
        None,
        "an unwrapped block came back with a width"
    );
}

/// A file written before text layers existed is not malformed; it simply has none.
#[test]
fn a_file_without_the_text_table_still_loads() {
    let f = TempFile::new("no-text-table");
    let doc = sample();
    save(f.path(), &doc, [], &[]).expect("save");
    {
        let db = rusqlite::Connection::open(f.path()).expect("reopen");
        db.execute_batch("DROP TABLE layer_text;")
            .expect("drop the table");
        db.pragma_update(None, "user_version", 5)
            .expect("downgrade");
    }

    let back = load(f.path()).expect("a v5 file should still load");
    assert_eq!(
        back.document.active().layer_count(),
        doc.active().layer_count()
    );
    assert!(
        back.document
            .active()
            .layers()
            .iter()
            .all(|l| l.text().is_none()),
        "a file with no text table should produce no text layers"
    );
}

/// Saving over a v5 file has to work, which is the case that broke when `layer_text` first landed:
/// the migration recreated the structure tables without dropping the new one.
#[test]
fn a_v5_file_can_be_saved_over() {
    let f = TempFile::new("v5-resave");
    save(f.path(), &sample(), [], &[]).expect("save");
    {
        let db = rusqlite::Connection::open(f.path()).expect("reopen");
        db.pragma_update(None, "user_version", 5)
            .expect("downgrade");
    }

    let mut doc = Document::new(Page::new(300, 300));
    doc.add_text_layer(TextBlock {
        text: "added after the migration".into(),
        ..TextBlock::default()
    });
    save(f.path(), &doc, [], &[]).expect("save over a v5 file");

    let back = load(f.path()).expect("load");
    assert_eq!(
        back.document
            .active()
            .layers()
            .iter()
            .find_map(openpaint_core::Layer::text)
            .expect("text layer")
            .text,
        "added after the migration"
    );
}
