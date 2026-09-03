//! Copy and paste, across the edge of the application.
//!
//! There was no clipboard at all before this: not within OpenPaint and not with the system. Copy,
//! cut and paste are muscle memory rather than features — nobody reads a menu to find out whether
//! Ctrl+C works — so their absence read as breakage, and it cut the application off from every
//! other program on the machine. A reference pasted from a browser and a panel pasted into a
//! message are both the point.
//!
//! # The system's clipboard, not one of our own
//!
//! An internal clipboard would be easier and would be the wrong thing: the whole value is that
//! the pixels can leave. Rust has no clipboard in its standard library and every platform's is
//! different, which is exactly what a crate is for — `arboard`, which speaks the Windows, X11,
//! Wayland and macOS ones.
//!
//! # The form the pixels travel in
//!
//! [`Picture`](crate::import::Picture): straight sRGB with alpha, which is what every clipboard
//! on every platform means by an image, and what an imported file already arrives as. Deliberately
//! the same type — a picture is a picture whether it came from a file or from another program,
//! and giving them separate types would mean two conversions into canvas tiles that could
//! disagree about premultiplication.

use crate::import::Picture;

/// Why the clipboard could not be used.
///
/// **Nothing here is the artist's fault**, which is why each one says what happened rather than
/// what they did wrong: a clipboard can be held open by another program, or hold something that
/// is not a picture at all.
#[derive(Debug)]
pub enum Trouble {
    /// The platform's clipboard would not open, or would not answer.
    Unavailable(String),
    /// It holds something, and that something is not an image.
    NotAPicture,
    /// It holds an image whose size and buffer disagree, or that no page could hold.
    Unusable(crate::import::Trouble),
}

impl std::fmt::Display for Trouble {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(e) => write!(f, "the clipboard could not be reached ({e})"),
            Self::NotAPicture => write!(f, "there is no picture on the clipboard"),
            Self::Unusable(e) => write!(f, "{e}"),
        }
    }
}

/// Put a picture on the system clipboard.
///
/// # Errors
/// [`Trouble::Unavailable`] if the platform's clipboard would not take it.
pub fn put(picture: &Picture) -> Result<(), Trouble> {
    let mut board = open()?;
    board
        .set_image(arboard::ImageData {
            width: picture.width() as usize,
            height: picture.height() as usize,
            bytes: std::borrow::Cow::Borrowed(picture.rgba()),
        })
        .map_err(|e| Trouble::Unavailable(e.to_string()))
}

/// Take the picture the system clipboard is holding.
///
/// # Errors
/// [`Trouble::NotAPicture`] when it holds text or nothing; [`Trouble::Unusable`] when the image it
/// holds is one no page could take.
pub fn take() -> Result<Picture, Trouble> {
    let mut board = open()?;
    let image = board.get_image().map_err(|e| match e {
        // Told apart on purpose. "There is nothing on the clipboard" and "the clipboard is broken"
        // are different sentences, and only one of them is about something the artist can fix by
        // copying something.
        arboard::Error::ContentNotAvailable | arboard::Error::ConversionFailure => {
            Trouble::NotAPicture
        }
        other => Trouble::Unavailable(other.to_string()),
    })?;
    let (width, height) = (image.width, image.height);
    #[expect(
        clippy::cast_possible_truncation,
        reason = "checked against MAX_PAGE_DIMENSION by Picture::new"
    )]
    Picture::new(width as u32, height as u32, image.bytes.into_owned()).map_err(Trouble::Unusable)
}

fn open() -> Result<arboard::Clipboard, Trouble> {
    arboard::Clipboard::new().map_err(|e| Trouble::Unavailable(e.to_string()))
}
