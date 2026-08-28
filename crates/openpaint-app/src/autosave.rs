//! Autosave and crash recovery.
//!
//! The feature that actually protects work. Explicit saving protects the work you remembered to
//! save; this protects the hour you were absorbed in.
//!
//! # A recovery copy is an ordinary document
//!
//! It is written with [`openpaint_file::save`], so it is a real `.openpaint` file that the app —
//! or `sqlite3`, or a future version — can open with no special path. There is no second format to
//! keep in step, and no risk of the recovery path rotting because nothing else exercises it. This
//! is the reason DECISIONS §7 chose a database over a zip: a zip would have made autosave a
//! full multi-hundred-megabyte rewrite each time.
//!
//! Two `meta` keys mark it: [`IS_RECOVERY`], and [`ORIGINAL_PATH`] when the document had a file.
//! The second is what lets recovery hand back a document that still knows where it belongs, rather
//! than an untitled orphan.
//!
//! # Where the copies live, and why not next to the document
//!
//! The OS per-user data directory. The document's own folder is the tempting choice and a bad one:
//! it may be read-only, on removable media that is no longer present, or inside a sync folder that
//! would happily propagate a half-written temporary file to every other machine. It is also not
//! ours to litter.
//!
//! # How a crash is detected
//!
//! By a recovery copy still existing. A copy is written only while there are unsaved changes, and
//! removed the moment there are not — on save, on load, on new, and on exit. So a copy present at
//! startup means the process that owned it did not get to clean up.
//!
//! **Known limitation:** two instances running at once. Each writes its own uniquely named copy,
//! but a second instance starting will see the first one's file and offer to recover a document
//! that is still open elsewhere. Accepting produces a duplicate of live work, which is untidy but
//! destroys nothing. A proper fix needs an OS file lock to distinguish "abandoned" from "in use";
//! it is not worth a dependency until multiple windows are actually supported.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// How long between autosaves.
///
/// A ceiling on how much work a crash can cost, so shorter is better and the only thing pushing
/// back is what a save costs. The panel reports that cost precisely so this can be revisited on
/// evidence rather than taste: a full save reads every resident tile back off the GPU, and if that
/// turns out to be expensive on a large document then the answer is an incremental save — the
/// format stores tiles as individually keyed rows, so only the changed ones need writing — not a
/// longer interval.
const INTERVAL: Duration = Duration::from_secs(60);

/// Marks a file as a recovery copy rather than something the user saved.
pub const IS_RECOVERY: &str = "autosave.recovery";

/// The document this recovery copy belongs to, absent if it was never saved.
pub const ORIGINAL_PATH: &str = "autosave.original_path";

/// An abandoned recovery copy found at startup.
pub struct Recoverable {
    /// Where the copy is.
    pub path: PathBuf,
    /// When it was last written.
    pub modified: SystemTime,
    /// The document it belongs to, if it had one.
    pub original: Option<PathBuf>,
}

impl Recoverable {
    /// A description for the prompt: what the work was, and how old it is.
    #[must_use]
    pub fn describe(&self) -> String {
        let what = self.original.as_ref().map_or_else(
            || "an unsaved document".to_owned(),
            |p| {
                p.file_name()
                    .map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into())
            },
        );
        match self.modified.elapsed() {
            Ok(age) => format!("{what}, last saved {}", human_age(age)),
            // A clock that moved backwards is not worth a branch in the UI.
            Err(_) => what,
        }
    }
}

/// Age as something a person reads, rather than a duration.
fn human_age(age: Duration) -> String {
    let secs = age.as_secs();
    match secs {
        0..=90 => format!("{secs} seconds ago"),
        91..=5400 => format!("{} minutes ago", (secs + 30) / 60),
        _ => format!("{} hours ago", (secs + 1800) / 3600),
    }
}

/// Writes periodic recovery copies, and finds abandoned ones.
pub struct Autosave {
    /// This session's copy, or `None` if the OS offered nowhere to put it.
    file: Option<PathBuf>,
    /// When the next autosave becomes due.
    due_at: Instant,
    /// The last write: when, how long it took, and how many tiles it covered.
    last: Option<(SystemTime, Duration, usize)>,
    /// Whether a copy is currently on disk.
    live: bool,
}

impl Autosave {
    pub fn new() -> Self {
        Self {
            file: session_file(),
            // Not due immediately: a freshly opened document has nothing to protect, and the first
            // interval is also grace for anyone who opens the app and closes it again.
            due_at: Instant::now() + INTERVAL,
            last: None,
            live: false,
        }
    }

    /// Whether it is time to write, given whether there is anything to protect.
    ///
    /// `drawing` is taken because a save reads tiles back off the GPU, which mid-stroke would both
    /// hitch the one thing that must never hitch and capture a stroke halfway through.
    #[must_use]
    pub fn is_due(&self, dirty: bool, drawing: bool) -> bool {
        dirty && !drawing && self.file.is_some() && Instant::now() >= self.due_at
    }

    /// Where this session's copy goes.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.file.as_deref()
    }

    /// Note a successful write.
    pub fn record(&mut self, took: Duration, tiles: usize) {
        self.last = Some((SystemTime::now(), took, tiles));
        self.live = true;
        self.due_at = Instant::now() + INTERVAL;
    }

    /// Note a failed write and try again later rather than every frame.
    pub fn postpone(&mut self) {
        self.due_at = Instant::now() + INTERVAL;
    }

    /// Throw away this session's copy, because there is nothing unsaved to recover.
    ///
    /// Called whenever the document becomes clean and on exit — which is what makes a surviving
    /// copy mean "this process died" rather than merely "this process ran".
    pub fn discard(&mut self) {
        if let Some(path) = self.file.as_deref() {
            if self.live {
                // A failure here is not worth telling anyone about: the worst outcome is being
                // offered a recovery that turns out to be identical to the saved file.
                let _ = std::fs::remove_file(path);
            }
        }
        self.live = false;
        self.due_at = Instant::now() + INTERVAL;
    }

    /// The last write, for display.
    #[must_use]
    pub fn last(&self) -> Option<(SystemTime, Duration, usize)> {
        self.last
    }

    /// Whether autosave is working at all. False means the OS gave us nowhere to write.
    #[must_use]
    pub fn available(&self) -> bool {
        self.file.is_some()
    }

    /// Find an abandoned recovery copy, newest first.
    #[must_use]
    pub fn find_recoverable(&self) -> Option<Recoverable> {
        scan(&recovery_dir()?, self.file.as_deref())
    }
}

/// Pick the newest abandoned recovery copy in `dir`, ignoring `ours`.
///
/// Split out from [`Autosave::find_recoverable`] so it can be tested against a directory we
/// control. It has three real ways to be wrong — offering our own live copy, offering a file that
/// is not a recovery copy at all, and offering the older of two — and none of them would be
/// noticed by hand until the day they mattered.
fn scan(dir: &Path, ours: Option<&Path>) -> Option<Recoverable> {
    let mut best: Option<Recoverable> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if Some(path.as_path()) == ours {
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some(crate::DOCUMENT_EXTENSION) {
            continue;
        }
        let Ok(modified) = entry.metadata().and_then(|m| m.modified()) else {
            continue;
        };
        // Read only the metadata table, not the tiles: this runs at startup for every candidate,
        // and the pixels are three orders of magnitude more expensive.
        let meta = openpaint_file::read_meta(&path).unwrap_or_default();
        if !meta.contains_key(IS_RECOVERY) {
            // Someone else's document that happens to live here, or a copy written before the
            // marker existed. Either way not ours to offer.
            continue;
        }
        let candidate = Recoverable {
            path,
            modified,
            original: meta.get(ORIGINAL_PATH).map(PathBuf::from),
        };
        if best
            .as_ref()
            .is_none_or(|b| candidate.modified > b.modified)
        {
            best = Some(candidate);
        }
    }
    best
}

impl Default for Autosave {
    fn default() -> Self {
        Self::new()
    }
}

/// The directory recovery copies live in, created if needed.
fn recovery_dir() -> Option<PathBuf> {
    let dir = dirs::data_local_dir()?.join("OpenPaint").join("recovery");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// A unique file name for this session's copy.
fn session_file() -> Option<PathBuf> {
    let dir = recovery_dir()?;
    // Wall-clock nanoseconds, not the process id. A pid is reused, and a new process that happened
    // to be given the pid of a crashed one would treat that crash's copy as its own and quietly
    // overwrite the very work it was meant to protect.
    let unique = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    Some(dir.join(format!("session-{unique}.{}", crate::DOCUMENT_EXTENSION)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_to_protect_means_nothing_to_do() {
        let mut a = Autosave::new();
        // Force the interval to have elapsed.
        a.due_at = Instant::now();
        assert!(
            !a.is_due(false, false),
            "a clean document has nothing worth writing"
        );
        assert!(
            !a.is_due(true, true),
            "mid-stroke is exactly when a GPU readback must not happen"
        );
        if a.available() {
            assert!(a.is_due(true, false), "dirty and idle is the case to write");
        }
    }

    #[test]
    fn a_write_pushes_the_next_one_out() {
        let mut a = Autosave::new();
        a.due_at = Instant::now();
        a.record(Duration::from_millis(12), 7);
        assert!(
            !a.is_due(true, false),
            "it should not be due again immediately after writing"
        );
        assert_eq!(a.last().map(|l| l.2), Some(7));
    }

    /// A failed write must not turn into a write attempt every frame.
    #[test]
    fn a_failure_backs_off_too() {
        let mut a = Autosave::new();
        a.due_at = Instant::now();
        a.postpone();
        assert!(!a.is_due(true, false));
    }

    #[test]
    fn ages_read_as_english() {
        assert_eq!(human_age(Duration::from_secs(4)), "4 seconds ago");
        assert_eq!(human_age(Duration::from_secs(120)), "2 minutes ago");
        assert_eq!(human_age(Duration::from_secs(7200)), "2 hours ago");
    }

    /// The description is what the user reads before deciding, so it has to name the work.
    #[test]
    fn a_recovery_names_the_document_it_came_from() {
        let named = Recoverable {
            path: PathBuf::from("/tmp/session-1.openpaint"),
            modified: SystemTime::now(),
            original: Some(PathBuf::from("/home/me/art/sketch.openpaint")),
        };
        assert!(
            named.describe().contains("sketch.openpaint"),
            "got {:?}",
            named.describe()
        );

        let untitled = Recoverable {
            path: PathBuf::from("/tmp/session-2.openpaint"),
            modified: SystemTime::now(),
            original: None,
        };
        assert!(
            untitled.describe().contains("unsaved"),
            "a document that never had a file still has to be describable: {:?}",
            untitled.describe()
        );
    }

    /// Two sessions must not share a file, or one would overwrite the other's only copy.
    #[test]
    fn sessions_do_not_collide() {
        let a = session_file();
        std::thread::sleep(Duration::from_millis(2));
        let b = session_file();
        if a.is_some() {
            assert_ne!(a, b);
        }
    }

    /// Discovery, against real files written by the real format.
    ///
    /// Three ways this can be wrong, all of them silent: offering our own live copy back to us,
    /// offering an ordinary document that merely happens to be in the directory, and offering the
    /// older of two crashes. The prompt is the one chance the artist gets, so it has to name the
    /// right file.
    #[test]
    fn discovery_picks_the_newest_abandoned_copy() {
        let dir = std::env::temp_dir().join(format!(
            "openpaint-recovery-test-{}",
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");

        let doc = openpaint_core::Document::new(openpaint_core::Page::new(64, 64));
        let write = |name: &str, meta: &[(&str, &str)]| {
            let path = dir.join(name);
            openpaint_file::save(&path, &doc, [], meta).expect("write candidate");
            path
        };

        let older = write(
            "session-1.openpaint",
            &[(IS_RECOVERY, "1"), (ORIGINAL_PATH, "/art/older.openpaint")],
        );
        // Filesystem timestamps are coarse, so make the ordering unambiguous rather than hoping.
        std::thread::sleep(Duration::from_millis(20));
        let newer = write(
            "session-2.openpaint",
            &[(IS_RECOVERY, "1"), (ORIGINAL_PATH, "/art/newer.openpaint")],
        );
        std::thread::sleep(Duration::from_millis(20));
        // An ordinary saved document: newest of all, and must be ignored entirely.
        let innocent = write("someones-artwork.openpaint", &[]);
        // And our own live copy, which must never be offered back to us.
        let ours = write("session-3.openpaint", &[(IS_RECOVERY, "1")]);

        let found = scan(&dir, Some(&ours)).expect("an abandoned copy should have been found");
        assert_eq!(
            found.path, newer,
            "expected the newest abandoned copy; older was {older:?}, innocent was {innocent:?}"
        );
        assert_eq!(
            found.original.as_deref(),
            Some(Path::new("/art/newer.openpaint")),
            "the copy has to remember which document it belongs to"
        );

        // With every marked copy accounted for, there is nothing to offer -- an unmarked document
        // is not a candidate however new it is.
        assert!(
            scan(&dir, Some(&newer)).is_some(),
            "the older one is still a candidate"
        );
        std::fs::remove_file(&older).ok();
        std::fs::remove_file(&newer).ok();
        assert!(
            scan(&dir, Some(&ours)).is_none(),
            "an ordinary saved document was offered as recoverable"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
