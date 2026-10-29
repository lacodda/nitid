//! One window, however many times the shell launches the viewer.
//!
//! Double-clicking a second image starts a second `nitid.exe`, and starting a
//! process means paying for a window and a GPU device again — measured at 190
//! to 340 milliseconds of the 320 to 560 a cold start costs. A viewer that is
//! already open has both, so the second process hands its file over and exits,
//! and the picture appears in tens of milliseconds instead.
//!
//! The same mechanism answers multi-select. The shell registers one file per
//! launch (`"nitid.exe" "%1"`), so selecting five images and pressing Enter
//! starts five processes; four of them hand their file to the first and quit,
//! and the five arrive as one list in one window.
//!
//! The channel is a named pipe. The first process to create it owns the
//! window; anyone who finds it already there is a messenger. That makes the
//! race resolve itself — creating a named pipe instance is atomic, so exactly
//! one process can be first — without a separate lock to go stale.

#[cfg(windows)]
pub mod channel;

use std::path::{Path, PathBuf};

/// The wire format: magic, version, what is asked, count, then that many
/// length-prefixed UTF-8 paths.
///
/// Deliberately dull, for the same reason the sandbox protocol is: whatever
/// connects to this pipe is another process on this machine, and while it runs
/// as the same user and could do worse things directly, a viewer that can be
/// made to allocate four gigabytes by a malformed header is still a bug.
const MAGIC: [u8; 4] = *b"NTD1";

/// Bumped when the shape of a message changes. Both ends ship in the same
/// executable, so a mismatch means something else is on the pipe: refuse
/// rather than negotiate.
///
/// 2 added what is asked: a picture to open, or one to look at from Explorer.
const VERSION: u16 = 2;

/// The most paths one message may carry.
///
/// Selecting a whole folder is a plausible accident; a message claiming four
/// billion is not. The cap is high enough that no real selection meets it.
const MAX_PATHS: u32 = 4096;

/// The longest single path the message may carry, in bytes.
///
/// Windows paths reach 32767 characters with the long-path prefix; this is
/// comfortably past that in UTF-8 and still far from an allocation worth
/// worrying about.
const MAX_PATH_BYTES: u32 = 64 * 1024;

/// What a messenger asks of the window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Kind {
    /// Open these: a double-click, a command line, "Open with nitid".
    #[default]
    Open,
    /// Look at these, from Explorer's space bar: the same pictures, in a
    /// window that the space bar sends back again (see `resident`).
    Peek,
}

impl Kind {
    fn byte(self) -> u8 {
        match self {
            Self::Open => 0,
            Self::Peek => 1,
        }
    }

    fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            0 => Some(Self::Open),
            1 => Some(Self::Peek),
            _ => None,
        }
    }
}

/// One message: the paths, and what is asked of them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Handover {
    pub paths: Vec<PathBuf>,
    pub kind: Kind,
}

/// Encode a hand-over message: the paths a second process wants shown.
pub fn encode(paths: &[PathBuf], kind: Kind) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.push(kind.byte());
    out.extend_from_slice(&(paths.len() as u32).to_le_bytes());
    for path in paths {
        let bytes = path.to_string_lossy();
        let bytes = bytes.as_bytes();
        out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(bytes);
    }
    out
}

/// Decode a hand-over message.
///
/// `None` for anything that is not one: a truncated read, a foreign sender, a
/// version that is not ours, or a length that would have this process
/// allocating on a stranger's say-so. The window carries on either way — a
/// bad message is not a reason to stop showing the picture that is up.
pub fn decode(bytes: &[u8]) -> Option<Handover> {
    let mut rest = bytes;

    let magic = take(&mut rest, 4)?;
    if magic != MAGIC {
        return None;
    }
    if u16::from_le_bytes(take(&mut rest, 2)?.try_into().ok()?) != VERSION {
        return None;
    }
    // Something this build does not know how to answer is refused whole, not
    // read as the nearest thing it does.
    let kind = Kind::from_byte(*take(&mut rest, 1)?.first()?)?;

    let count = u32::from_le_bytes(take(&mut rest, 4)?.try_into().ok()?);
    if count > MAX_PATHS {
        return None;
    }

    let mut paths = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let length = u32::from_le_bytes(take(&mut rest, 4)?.try_into().ok()?);
        if length > MAX_PATH_BYTES {
            return None;
        }
        let text = std::str::from_utf8(take(&mut rest, length as usize)?).ok()?;
        paths.push(PathBuf::from(text));
    }

    // Trailing bytes mean the sender is not speaking this protocol.
    rest.is_empty().then_some(Handover { paths, kind })
}

/// Split `count` bytes off the front, or `None` if they are not there.
fn take<'a>(rest: &mut &'a [u8], count: usize) -> Option<&'a [u8]> {
    if rest.len() < count {
        return None;
    }
    let (head, tail) = rest.split_at(count);
    *rest = tail;
    Some(head)
}

/// The pipe every instance looks for.
///
/// Per user, because two people signed into one machine each get their own
/// viewer: a pipe is machine-wide, so the name has to carry the distinction
/// the pipe itself does not. `NITID_INSTANCE_ID` narrows it further, which is
/// how the tests get a channel of their own rather than talking to whatever
/// window the developer has open.
pub fn pipe_name() -> String {
    let user = std::env::var("USERNAME").unwrap_or_else(|_| "user".into());
    format!(r"\\.\pipe\nitid-{}{}", sanitise(&user), instance_tag())
}

/// What `NITID_INSTANCE_ID` adds to every name the instances find each other
/// by — the pipe here, the resident's window and lock — so a test run has
/// names of its own. Empty for everyone else.
pub fn instance_tag() -> String {
    sanitise(&std::env::var("NITID_INSTANCE_ID").unwrap_or_default())
}

/// Keep a pipe name to characters that cannot end the path or start a new one.
///
/// A username is not attacker-controlled here, but it does contain spaces and
/// non-ASCII in the ordinary case, and a backslash in it would silently name a
/// different pipe.
fn sanitise(text: &str) -> String {
    text.chars()
        .map(|character| if character.is_ascii_alphanumeric() { character } else { '-' })
        .collect()
}

/// How long a messenger waits for a listener that is busy with someone else.
///
/// Multi-select is a burst: five processes start at once and the window serves
/// them one at a time, so a messenger that gave up immediately would open a
/// second window for no better reason than arriving second. Generous, because
/// the wait only happens when a window is definitely there — "nobody is
/// listening" is answered at once and costs a cold start nothing.
pub const HANDOVER_PATIENCE: std::time::Duration = std::time::Duration::from_secs(10);

/// Whether this build should try to share a window at all.
///
/// One window is what a viewer should do, so it is the default. The lever
/// exists because the startup gate measures a *cold* start: if it handed its
/// file to a window left over from the previous test, it would report a number
/// that has nothing to do with what a user experiences.
pub fn enabled() -> bool {
    !std::env::var_os("NITID_NO_SINGLE_INSTANCE").is_some_and(|value| value != "0")
}

/// Make the paths absolute before they cross to another process.
///
/// The two processes can have different working directories — the shell sets
/// one per launch — so a relative path means something different on the other
/// side. Resolved here, where the original directory still applies.
pub fn absolute(paths: &[PathBuf]) -> Vec<PathBuf> {
    paths.iter().map(|path| std::path::absolute(path).unwrap_or_else(|_| path.clone())).collect()
}

/// Whether a path is worth handing over: it must exist, as a file or as a
/// folder to walk.
///
/// A messenger that sends nonsense would make the window jump to an error for
/// a file the user never chose.
pub fn openable(path: &Path) -> bool {
    path.is_file() || path.is_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open(paths: &[PathBuf]) -> Vec<u8> {
        encode(paths, Kind::Open)
    }

    fn paths_of(bytes: &[u8]) -> Option<Vec<PathBuf>> {
        decode(bytes).map(|handover| handover.paths)
    }

    /// What is asked crosses with the paths: a look from Explorer must not
    /// arrive as an ordinary open, or the space bar would not send the
    /// window back.
    #[test]
    fn what_is_asked_survives_the_round_trip() {
        let paths = vec![PathBuf::from(r"C:\pictures\one.jpg")];
        for kind in [Kind::Open, Kind::Peek] {
            assert_eq!(decode(&encode(&paths, kind)), Some(Handover { paths: paths.clone(), kind }));
        }
    }

    /// A request this build does not know is refused, not taken for an open.
    #[test]
    fn an_unknown_request_is_refused() {
        let mut bytes = open(&[PathBuf::from("a.jpg")]);
        bytes[6] = 7;
        assert!(decode(&bytes).is_none());
    }

    #[test]
    fn a_message_survives_the_round_trip() {
        let paths = vec![PathBuf::from(r"C:\pictures\one.jpg"), PathBuf::from(r"C:\pictures\two.png")];
        let decoded = paths_of(&open(&paths)).expect("its own message");
        assert_eq!(decoded, paths);
    }

    #[test]
    fn an_empty_message_is_valid_and_carries_nothing() {
        assert_eq!(paths_of(&open(&[])), Some(vec![]));
    }

    #[test]
    fn a_path_with_spaces_and_non_ascii_survives() {
        let paths = vec![PathBuf::from(r"C:\снимки\мой файл.jpg")];
        assert_eq!(paths_of(&open(&paths)).as_deref(), Some(paths.as_slice()));
    }

    #[test]
    fn a_foreign_sender_is_refused() {
        let mut bytes = open(&[PathBuf::from("a.jpg")]);
        bytes[0] = b'X';
        assert!(paths_of(&bytes).is_none());
    }

    #[test]
    fn another_version_is_refused_rather_than_guessed_at() {
        let mut bytes = open(&[PathBuf::from("a.jpg")]);
        bytes[4..6].copy_from_slice(&99u16.to_le_bytes());
        assert!(paths_of(&bytes).is_none());
    }

    #[test]
    fn a_truncated_message_is_refused() {
        let bytes = open(&[PathBuf::from("a.jpg")]);
        for cut in 0..bytes.len() {
            assert!(paths_of(&bytes[..cut]).is_none(), "a message cut to {cut} bytes was accepted");
        }
    }

    #[test]
    fn trailing_bytes_are_refused() {
        let mut bytes = open(&[PathBuf::from("a.jpg")]);
        bytes.push(0);
        assert!(paths_of(&bytes).is_none());
    }

    /// A header claiming a count, with no paths behind it.
    fn header_claiming(count: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&MAGIC);
        bytes.extend_from_slice(&VERSION.to_le_bytes());
        bytes.push(Kind::Open.byte());
        bytes.extend_from_slice(&count.to_le_bytes());
        bytes
    }

    /// The header says how much to allocate, and the header comes from
    /// another process. The cap has to be the cap, not merely something
    /// below four billion: tested at the boundary, because a test that only
    /// tries `u32::MAX` passes just as happily against a limit a thousand
    /// times too loose.
    #[test]
    fn the_count_is_capped_at_exactly_the_stated_limit() {
        // One past the cap: refused outright, before any allocation.
        assert!(paths_of(&header_claiming(MAX_PATHS + 1)).is_none());
        assert!(paths_of(&header_claiming(u32::MAX)).is_none());

        // At the cap: not refused for being too many. It still fails, because
        // the paths themselves are missing — but on truncation, which is what
        // a genuine message of this size would be checked for.
        let at_limit = paths_of(&header_claiming(MAX_PATHS));
        assert!(at_limit.is_none(), "the header alone is not a whole message");

        // A real message of the largest allowed size round-trips.
        let paths: Vec<PathBuf> = (0..MAX_PATHS).map(|index| PathBuf::from(format!("{index}.jpg"))).collect();
        assert_eq!(paths_of(&open(&paths)).as_deref(), Some(paths.as_slice()));

        // One path more than the cap is refused, however well-formed.
        let too_many: Vec<PathBuf> = (0..=MAX_PATHS).map(|index| PathBuf::from(format!("{index}.jpg"))).collect();
        assert!(paths_of(&open(&too_many)).is_none(), "a selection past the cap was accepted");
    }

    /// The same for one path's length, and for the same reason.
    #[test]
    fn a_path_length_is_capped_at_exactly_the_stated_limit() {
        let claiming = |length: u32| {
            let mut bytes = header_claiming(1);
            bytes.extend_from_slice(&length.to_le_bytes());
            bytes
        };

        assert!(paths_of(&claiming(MAX_PATH_BYTES + 1)).is_none());
        assert!(paths_of(&claiming(u32::MAX)).is_none());

        // A path exactly at the cap is carried.
        let long = PathBuf::from("x".repeat(MAX_PATH_BYTES as usize));
        assert_eq!(paths_of(&open(std::slice::from_ref(&long))).as_deref(), Some(std::slice::from_ref(&long)));

        // One byte past it is not.
        let longer = PathBuf::from("x".repeat(MAX_PATH_BYTES as usize + 1));
        assert!(paths_of(&open(&[longer])).is_none(), "a path past the cap was accepted");
    }

    #[test]
    fn a_long_but_legal_selection_survives() {
        let paths: Vec<PathBuf> = (0..500).map(|index| PathBuf::from(format!(r"C:\pictures\{index}.jpg"))).collect();
        assert_eq!(paths_of(&open(&paths)).as_deref(), Some(paths.as_slice()));
    }

    #[test]
    fn the_pipe_name_is_a_pipe_path_with_no_stray_separators() {
        let name = pipe_name();
        assert!(name.starts_with(r"\\.\pipe\"), "{name} is not a pipe path");
        // Exactly the three separators of the prefix: a fourth would name a
        // different pipe than intended.
        assert_eq!(name.matches('\\').count(), 4, "{name} carries a separator from the username");
    }

    #[test]
    fn a_username_with_a_separator_cannot_change_which_pipe_is_named() {
        assert_eq!(sanitise(r"dom\ain user"), "dom-ain-user");
    }
}
