//! Mark a document as an abandoned recovery copy.
//!
//! **For driving the application, and for nothing else.** A recovery copy is an ordinary document
//! with one extra row in its `meta` table -- that is the whole design (see `autosave`'s module
//! header), and it is what lets the recovery path be the same code as loading anything else. The
//! consequence is that a test can make one without crashing the application on purpose, which is
//! otherwise the only way to find out whether the prompt that offers it works.
//!
//! The prompt is worth this trouble: it blocks every pen stroke while it is up, and a version of
//! it that was drawn nowhere but still refused input made the whole application look broken.

fn main() {
    let mut args = std::env::args_os().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: mark-recovery <document.openpaint>");
        std::process::exit(2);
    };
    let db = match rusqlite::Connection::open(std::path::Path::new(&path)) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("{}: {e}", path.to_string_lossy());
            std::process::exit(1);
        }
    };
    let done = db.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES ('autosave.recovery', '1')",
        [],
    );
    if let Err(e) = done {
        eprintln!("could not write the marker: {e}");
        std::process::exit(1);
    }
}
