//! Going through a shoot and saying which frames are worth keeping.
//!
//! The mark goes **into the file**, as the EXIF rating Windows itself writes
//! and reads (tag `0x4746` in IFD0, with `0x4749` alongside it as a
//! percentage). That is the whole point of the feature and it was settled by
//! measurement rather than by preference: a rating written this way shows up
//! as stars in Explorer's own column, and one written to an XMP file beside
//! the picture shows up as "Unrated". A sidecar is a mark only the program
//! that wrote it can see, which is the opposite of what culling is for — the
//! selection has to survive being handed to something else.
//!
//! The price is that the file is rewritten to carry the tag. It is the same
//! price the saved turn pays (`rotate`), and it is paid the same way: only the
//! metadata block is rebuilt, never the pixels, so a marked JPEG is identical
//! from its start-of-scan marker onward.
//!
//! Three marks rather than two flags. Keep and reject could each have been a
//! flag of its own, but then a file has two independent states and "show me
//! the marked ones" stops having one answer. They are one scale with three
//! positions, which is what the rating field already is.

use std::path::Path;

use anyhow::{Context, Result};
use little_exif::exif_tag::ExifTag;
use little_exif::ifd::ExifTagGroup;
use little_exif::metadata::Metadata;

/// EXIF `Rating`, IFD0. What Explorer reads for its star column.
const RATING: u16 = 0x4746;

/// EXIF `RatingPercent`, IFD0. Windows writes both and so does this; a reader
/// that prefers the percentage gets one that agrees with the stars.
const RATING_PERCENT: u16 = 0x4749;

/// What has been said about a picture while going through a folder.
///
/// The names are the judgement, not the number: a person culling a shoot
/// thinks "keep this one", not "one star". The numbers are how the judgement
/// survives being written down.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Mark {
    /// Nothing said about it yet. Not a verdict — the frames nobody has
    /// reached are in this state, and so are the ones deliberately cleared.
    #[default]
    Unmarked,
    /// Worth keeping.
    Keep,
    /// Not worth keeping.
    ///
    /// A mark, emphatically not a delete. `Del` already sends a file to the
    /// recycle bin and always has; the point of a reject mark is to get
    /// through the folder *without* stopping to decide anything irreversible,
    /// and then look at what came out.
    Reject,
}

impl Mark {
    /// The rating a file carries for this mark.
    ///
    /// Lightroom's reject is a flag beside the rating rather than a rating,
    /// and EXIF has nowhere to put it: the field is `0..=5` and Windows shows
    /// anything outside that as unrated. So reject is recorded as the one
    /// value a person would never set by hand — the percentage at zero with
    /// the stars at zero is indistinguishable from unmarked, which is why the
    /// two are told apart by the percentage instead. See [`from_exif`].
    fn stars(self) -> u16 {
        match self {
            Self::Unmarked | Self::Reject => 0,
            Self::Keep => 1,
        }
    }

    /// The percentage written beside the stars.
    ///
    /// `1` for a reject: out of the range Windows maps to any star, so
    /// Explorer still says "Unrated", while a reader that looks at the
    /// percentage can tell a rejected frame from an untouched one. Windows
    /// writes 1/25/50/75/99 for one through five stars, so 1 is not free —
    /// but it is only reachable with the stars at zero, which Windows never
    /// writes, and that pair is what identifies a reject.
    fn percent(self) -> u16 {
        match self {
            Self::Unmarked => 0,
            Self::Keep => 1,
            Self::Reject => 1,
        }
    }

    /// What the status line and a toast call it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Unmarked => "unmarked",
            Self::Keep => "keep",
            Self::Reject => "reject",
        }
    }

    /// Whether the filter lets this one through.
    ///
    /// Reject is a mark, so a folder filtered to "marked" shows the rejects
    /// too: the point of the filter is to see what the pass produced, and
    /// hiding the rejects would hide half the answer.
    pub fn is_marked(self) -> bool {
        !matches!(self, Self::Unmarked)
    }
}

/// Read the mark a file carries.
///
/// Never an error: a file with no EXIF, a format that cannot carry any, and a
/// file that cannot be opened at all are the ordinary cases of "nothing said
/// about it", not failures to report. A viewer that showed an error banner
/// while stepping through a folder of PNGs would be unusable.
pub fn read(path: &Path) -> Mark {
    let Ok(bytes) = std::fs::read(path) else {
        return Mark::Unmarked;
    };
    from_bytes(&bytes)
}

/// The same, from bytes already in hand.
///
/// The viewer has the file in memory when it shows it, so the panel and the
/// status line read the mark without going back to the disk.
pub fn from_bytes(bytes: &[u8]) -> Mark {
    let mut cursor = std::io::Cursor::new(bytes);
    let Ok(exif) = exif::Reader::new().read_from_container(&mut cursor) else {
        return Mark::Unmarked;
    };

    let field = |tag: u16| {
        exif.get_field(exif::Tag(exif::Context::Tiff, tag), exif::In::PRIMARY)
            .and_then(|field| field.value.get_uint(0))
    };

    from_exif(field(RATING), field(RATING_PERCENT))
}

/// Work out the mark from the two tags, either of which may be missing.
///
/// Split out from the reading so the rule itself can be tested over every
/// combination, including the ones no writer of ours produces: another
/// program's rating has to land somewhere sensible, and "somewhere sensible"
/// is a decision, not an accident.
fn from_exif(stars: Option<u32>, percent: Option<u32>) -> Mark {
    match (stars, percent) {
        // Our reject: no stars, but a percentage saying something was said.
        // Windows never writes this pair — its percentages come with stars —
        // so reading it back cannot mistake another program's file.
        (Some(0) | None, Some(1..)) => Mark::Reject,
        // Any star at all is a keep. One star is what this viewer writes;
        // two through five come from somewhere else and are still a keep,
        // because a person who rated a photograph in another program has
        // plainly said they want it.
        (Some(1..), _) => Mark::Keep,
        _ => Mark::Unmarked,
    }
}

/// Write the mark into the file, leaving its pixels alone.
///
/// Fails rather than quietly doing nothing when the format cannot carry EXIF:
/// a viewer that says it marked a file and did not is worse than one that says
/// the format has nowhere to put the mark. Same rule as the saved turn.
pub fn write(path: &Path, mark: Mark) -> Result<()> {
    // A file with no EXIF at all is the ordinary case for anything a phone did
    // not take, and `new_from_path` fails outright on one — an empty block is
    // the right starting point, and the write below puts it into the file.
    let mut metadata = Metadata::new_from_path(path).unwrap_or_else(|_| Metadata::new());
    metadata.set_tag(ExifTag::UnknownINT16U(vec![mark.stars()], RATING, ExifTagGroup::GENERIC));
    metadata.set_tag(ExifTag::UnknownINT16U(vec![mark.percent()], RATING_PERCENT, ExifTagGroup::GENERIC));
    metadata.write_to_file(path).with_context(|| format!("{} cannot carry a mark", name_of(path)))
}

/// What to call a path in a message: its last component.
fn name_of(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Somewhere to write files that is not the owner's disk proper.
    fn sandbox(name: &str) -> std::path::PathBuf {
        let directory = std::env::temp_dir().join("nitid-cull-tests").join(name);
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("a sandbox to write into");
        directory
    }

    /// A JPEG built here rather than committed: a fixture from the owner's
    /// archive would put a real photograph in a public repository.
    ///
    /// Noise at a quality the default encoder does not use, for the reason
    /// `rotate`'s fixture is: a smooth gradient at the default quality
    /// re-encodes to the same bytes, and a test comparing scan data could
    /// then not tell a re-encoding implementation from this one.
    fn a_jpeg(path: &Path) {
        let mut pixels = image::RgbImage::new(96, 64);
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        for pixel in pixels.pixels_mut() {
            let mut next = || {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                (seed >> 24) as u8
            };
            *pixel = image::Rgb([next(), next(), next()]);
        }
        let file = std::fs::File::create(path).expect("a file to write");
        let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(std::io::BufWriter::new(file), 93);
        encoder.encode_image(&pixels).expect("an encoded jpeg");
    }

    /// The compressed image data: everything from the start-of-scan marker on.
    fn scan_data(path: &Path) -> Vec<u8> {
        let bytes = std::fs::read(path).expect("the file back");
        bytes
            .windows(2)
            .position(|pair| pair == [0xFF, 0xDA])
            .map(|start| bytes[start..].to_vec())
            .expect("a jpeg has a start-of-scan marker")
    }

    #[test]
    fn a_mark_survives_being_written_and_read_back() {
        let directory = sandbox("round-trip");
        for mark in [Mark::Keep, Mark::Reject, Mark::Unmarked] {
            let path = directory.join(format!("{}.jpg", mark.name()));
            a_jpeg(&path);
            assert_eq!(read(&path), Mark::Unmarked, "a fresh file starts unmarked");

            write(&path, mark).expect("the mark goes in");
            assert_eq!(read(&path), mark, "{} did not come back", mark.name());
        }
    }

    /// The promise of the feature: the mark is written and the pixels are not
    /// touched.
    ///
    /// Mutating `write` into something that decodes and re-encodes would keep
    /// the round-trip test green and fail here.
    #[test]
    fn marking_a_file_leaves_its_pixels_exactly_as_they_were() {
        let directory = sandbox("lossless");
        let path = directory.join("photo.jpg");
        a_jpeg(&path);
        let before = scan_data(&path);

        write(&path, Mark::Keep).expect("the mark goes in");

        assert_eq!(scan_data(&path), before, "the compressed image data changed — something re-encoded the picture");
    }

    /// Changing one's mind has to be as cheap as the first press, including
    /// the way back to nothing.
    #[test]
    fn a_mark_can_be_changed_and_cleared() {
        let directory = sandbox("change");
        let path = directory.join("photo.jpg");
        a_jpeg(&path);

        write(&path, Mark::Keep).expect("keep");
        assert_eq!(read(&path), Mark::Keep);

        write(&path, Mark::Reject).expect("reject");
        assert_eq!(read(&path), Mark::Reject, "a reject did not replace the keep");

        write(&path, Mark::Unmarked).expect("clear");
        assert_eq!(read(&path), Mark::Unmarked, "clearing left the file marked");
    }

    /// The rule that tells the three apart, over every pair either tag can
    /// present — including the ones this viewer never writes.
    #[test]
    fn the_rating_of_another_program_lands_somewhere_sensible() {
        // What we write.
        assert_eq!(from_exif(Some(1), Some(1)), Mark::Keep);
        assert_eq!(from_exif(Some(0), Some(1)), Mark::Reject);
        assert_eq!(from_exif(Some(0), Some(0)), Mark::Unmarked);

        // What Windows writes: stars with a matching percentage. Every one of
        // them is a person saying they want the picture.
        for (stars, percent) in [(1, 1), (2, 25), (3, 50), (4, 75), (5, 99)] {
            assert_eq!(from_exif(Some(stars), Some(percent)), Mark::Keep, "{stars} stars should read as a keep");
        }

        // A file with neither tag, or with only one of them.
        assert_eq!(from_exif(None, None), Mark::Unmarked);
        assert_eq!(from_exif(None, Some(0)), Mark::Unmarked);
        assert_eq!(from_exif(Some(3), None), Mark::Keep, "stars alone are still a rating");
        assert_eq!(from_exif(None, Some(75)), Mark::Reject, "a percentage with no stars is our own reject");
    }

    /// The filter's question, answered for each of the three.
    #[test]
    fn the_filter_lets_through_everything_that_was_judged() {
        assert!(Mark::Keep.is_marked());
        assert!(
            Mark::Reject.is_marked(),
            "a rejected frame was judged — the filter has to show what the pass produced"
        );
        assert!(!Mark::Unmarked.is_marked());
    }

    /// Reading a file that is not there, and one that carries no EXIF, are
    /// both "nothing said about it" rather than errors.
    #[test]
    fn a_file_with_nothing_to_say_reads_as_unmarked() {
        let directory = sandbox("silent");
        assert_eq!(read(&directory.join("does-not-exist.jpg")), Mark::Unmarked);

        let path = directory.join("plain.png");
        image::RgbImage::new(4, 4).save(&path).expect("a png");
        assert_eq!(read(&path), Mark::Unmarked);
    }
}
