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
    let mut doc = Document::new(Page::new(1000, 1400), Mode::Continuous);
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

#[test]
fn a_document_round_trips_exactly() {
    let f = TempFile::new("structure");
    let doc = sample();
    save(f.path(), &doc, []).expect("save");
    let back = load(f.path()).expect("load");

    assert_eq!(back.document.page_count(), doc.page_count());
    assert_eq!(back.document.active_index(), doc.active_index());
    assert_eq!(back.document.mode(), doc.mode());

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
    save(f.path(), &doc, []).expect("save");
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

    save(f.path(), &doc, [(far, patterned(9))]).expect("save");
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
    save(f.path(), &doc, [(key, patterned(1))]).expect("first save");

    // A smaller document over the top of a larger one: the extra page must not linger.
    let doc2 = Document::new(Page::new(200, 200), Mode::Pages);
    save(f.path(), &doc2, [(key, patterned(2))]).expect("second save");

    let back = load(f.path()).expect("load");
    assert_eq!(back.document.page_count(), 1, "the old page survived");
    assert_eq!(back.document.mode(), Mode::Pages);
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
    save(f.path(), &sample(), []).expect("save");
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
        save(f.path(), &sample(), []),
        Err(Error::TooNew { .. })
    ));
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
    save(f.path(), &sample(), []).expect("save");
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
    let mut doc = Document::new(Page::new(400, 400), Mode::Pages);
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
    save(f.path(), &doc, [(a, patterned(1)), (b, patterned(2))]).expect("save");

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
    let mut doc = Document::new(Page::new(400, 400), Mode::Pages);
    doc.add_page_like_active();
    doc.active_mut().resize(PageRect::new(-50, -60, 900, 1500));
    doc.active_mut().set_dpi(600.0);
    doc.set_active(0);

    save(f.path(), &doc, []).expect("save");
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
    for mode in [Mode::Pages, Mode::Continuous] {
        assert_eq!(mode_from_name(mode_name(mode)), Some(mode));
    }
}
