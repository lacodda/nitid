//! What the thumbnail DLL and `nitid.exe` agree on.
//!
//! The DLL is built by itself, from `provider.rs` and this file and nothing
//! else (`build.rs`), so everything the two sides share is here: which class
//! the DLL is, how it starts `nitid.exe`, and what comes back.
//!
//! **The two can be of different versions.** A DLL an update replaced stays
//! loaded in whatever host was drawing thumbnails until that host goes, and it
//! starts the `nitid.exe` that is beside it, which is the new one. So the reply
//! opens with a marker and a version, and a side that does not know them draws
//! nothing.

// The DLL reads replies and the program writes them; each side leaves the
// other's half unused.
#![allow(dead_code)]

use std::io::{self, Write};

/// The DLL's class, by which the registry names it. Never changes: what an
/// earlier version registered is what a later one has to find and take away.
pub const CLASS: u128 = 0xe36f4421_0f9a_496e_993d_c6c773cbd261;

/// The program beside the DLL that does the work.
pub const PROGRAM: &str = "nitid.exe";

/// The first argument the DLL gives `nitid.exe`.
///
/// The decoder's own, with [`THUMB`] after it, rather than [`THUMB`] alone.
/// Somebody who installs an older release over this one keeps the DLL, and a
/// version that had not heard of thumbnails would take an argument it does not
/// know for the name of a picture and open a window on it - once for every
/// file in the folder. Every version there has been answers this one by
/// reading its input, saying what is wrong with it, and going.
pub const ASKED_FIRST: &str = "--decode-stdin";

/// What makes it a request for a thumbnail. The longest side it may have
/// follows; the picture arrives on standard input.
pub const THUMB: &str = "--thumb";

/// The longest side a thumbnail is asked for or given, in pixels: the largest
/// Explorer keeps.
pub const LONGEST: u32 = 2560;

/// The largest file a thumbnail is made from. The decoder's own limit on what
/// it will be handed (`sandbox::protocol::MAX_PAYLOAD`).
pub const LARGEST_FILE: u64 = 512 * 1024 * 1024;

const MAGIC: [u8; 4] = *b"NTTH";
const VERSION: u16 = 1;

/// Marker, version, width, height.
const HEADER: usize = 4 + 2 + 4 + 4;

/// The longest a reply can be, for whoever reads one.
pub const LARGEST_REPLY: u64 = HEADER as u64 + LONGEST as u64 * LONGEST as u64 * 4;

/// A thumbnail as a Windows bitmap holds it.
#[derive(Debug, PartialEq, Eq)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
    /// Blue, green, red, alpha, the colours multiplied by the alpha; top row
    /// first, `width * height * 4` bytes.
    pub pixels: Vec<u8>,
}

/// Say it.
pub fn write(mut out: impl Write, picture: &Picture) -> io::Result<()> {
    out.write_all(&MAGIC)?;
    out.write_all(&VERSION.to_le_bytes())?;
    out.write_all(&picture.width.to_le_bytes())?;
    out.write_all(&picture.height.to_le_bytes())?;
    out.write_all(&picture.pixels)?;
    out.flush()
}

/// Read one, no longer than `side` on either side. `None` for anything that is
/// not exactly that: a failure is an empty reply, and an older program answers
/// in another protocol altogether.
pub fn read(reply: &[u8], side: u32) -> Option<Picture> {
    let (header, pixels) = reply.split_at_checked(HEADER)?;
    if header[..4] != MAGIC || header[4..6] != VERSION.to_le_bytes() {
        return None;
    }
    let width = u32::from_le_bytes(header[6..10].try_into().ok()?);
    let height = u32::from_le_bytes(header[10..14].try_into().ok()?);
    let sound = |length: u32| (1..=side.min(LONGEST)).contains(&length);
    if !sound(width) || !sound(height) || pixels.len() != width as usize * height as usize * 4 {
        return None;
    }
    Some(Picture {
        width,
        height,
        pixels: pixels.to_vec(),
    })
}

/// [`CLASS`] as the registry spells one.
pub fn class_text() -> String {
    let digits = format!("{CLASS:032X}");
    format!(
        "{{{}-{}-{}-{}-{}}}",
        &digits[..8],
        &digits[8..12],
        &digits[12..16],
        &digits[16..20],
        &digits[20..]
    )
}
