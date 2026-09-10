//! Writing a turn back to the file.
//!
//! Only the orientation tag is rewritten, never the pixels. A JPEG rotated
//! this way is byte-identical from its start-of-scan marker onward — "without
//! loss" here is not a promise about a good encoder, it is the absence of an
//! encoder altogether. The same tag serves every format that carries EXIF, so
//! this is not a JPEG feature that happens to work elsewhere.
//!
//! The price, and it is a real one: a program that ignores EXIF shows the
//! picture the way it was. Every viewer, browser and phone gallery worth the
//! name honours the tag, and the alternative — permuting a JPEG's compressed
//! blocks — is JPEG-only, needs a C library this project deliberately refuses
//! (ADR 0002, ADR 0007), and trims edges that are not a multiple of the block
//! size. ADR 0024 records the choice.

use std::path::Path;

use anyhow::{Context, Result};
use little_exif::exif_tag::ExifTag;
use little_exif::metadata::Metadata;

use crate::image_source::Orientation;

/// Write `orientation` into the file's EXIF, leaving its pixels alone.
///
/// Formats that cannot carry EXIF fail rather than silently doing nothing:
/// a viewer that says it saved a turn and did not is worse than one that
/// admits the format has nowhere to put it.
pub fn save_orientation(path: &Path, orientation: Orientation) -> Result<()> {
    // A file with no EXIF at all is the ordinary case for anything a phone did
    // not take, and `new_from_path` fails outright on one — measured, not
    // assumed. An empty block is the right starting point there; the write
    // below puts it into the file.
    let mut metadata = Metadata::new_from_path(path).unwrap_or_else(|_| Metadata::new());
    metadata.set_tag(ExifTag::Orientation(vec![u16::from(orientation.to_exif())]));
    metadata.write_to_file(path).with_context(|| format!("{} cannot carry the turn", name_of(path)))
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
        let directory = std::env::temp_dir().join("nitid-rotate-tests").join(name);
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("a sandbox to write into");
        directory
    }

    /// A JPEG built here rather than committed: a fixture from the owner's
    /// archive would put a real photograph in a public repository.
    ///
    /// Encoded by `image`, which is a different encoder from the one under
    /// test — nothing here decodes it, so the file is only ever a carrier for
    /// the tag.
    fn a_jpeg(path: &Path) {
        // Noise, and encoded at a quality the default encoder does not use.
        //
        // Both details are load-bearing, and the first version of this test
        // had neither: a small smooth gradient at the default quality
        // re-encodes to the *same bytes*, so the check below could not tell a
        // re-encoding implementation from this one. Mutation testing caught
        // that — the mutation survived — and this is the fixture that kills it.
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
    ///
    /// This is what "without loss" means, and the only way to check it is to
    /// look at the bytes. A test that decoded both files and compared pixels
    /// would pass just as happily on a re-encoded image, which is the very
    /// thing this feature exists not to do.
    fn scan_data(path: &Path) -> Vec<u8> {
        let bytes = std::fs::read(path).expect("the file back");
        bytes
            .windows(2)
            .position(|pair| pair == [0xFF, 0xDA])
            .map(|start| bytes[start..].to_vec())
            .expect("a jpeg has a start-of-scan marker")
    }

    fn orientation_of(path: &Path) -> Option<u16> {
        let file = std::fs::File::open(path).ok()?;
        let mut reader = std::io::BufReader::new(file);
        let exif = exif::Reader::new().read_from_container(&mut reader).ok()?;
        exif.get_field(exif::Tag::Orientation, exif::In::PRIMARY)?
            .value
            .get_uint(0)
            .map(|value| value as u16)
    }

    /// The promise of the whole stage: the turn is saved and the pixels are
    /// not touched.
    ///
    /// Mutating `save_orientation` to decode and re-encode would keep the
    /// orientation assertion green and fail here, which is the point of
    /// comparing the compressed bytes rather than the picture.
    #[test]
    fn saving_a_turn_leaves_the_pixels_exactly_as_they_were() {
        let path = sandbox("untouched").join("picture.jpg");
        a_jpeg(&path);

        let before = scan_data(&path);
        save_orientation(&path, Orientation::Rotate90).expect("the turn to be saved");

        assert_eq!(orientation_of(&path), Some(6), "the turn was not written where every viewer looks for it");
        assert_eq!(
            scan_data(&path),
            before,
            "the compressed image data changed, so the picture was re-encoded rather than re-labelled",
        );
    }

    /// A file with no EXIF at all is the ordinary case, not an edge one.
    ///
    /// The crate's own `new_from_path` fails on such a file, so a version of
    /// this that trusted it would work only on photographs a phone had taken —
    /// which is exactly the kind of gap that reaches the owner rather than a
    /// test.
    #[test]
    fn a_file_that_carries_no_exif_can_still_be_turned() {
        let path = sandbox("bare").join("picture.jpg");
        a_jpeg(&path);
        assert_eq!(orientation_of(&path), None, "this fixture was supposed to start without any exif");

        save_orientation(&path, Orientation::Rotate270).expect("a file without exif to take a turn");

        assert_eq!(orientation_of(&path), Some(8), "a file that began without exif did not keep the turn");
    }

    /// Every one of the eight orientations survives the round trip.
    ///
    /// Written as a sweep rather than one example because the interesting
    /// values are the mirrored ones, which no ordinary photograph produces and
    /// a single case would never reach.
    #[test]
    fn every_orientation_survives_being_written_and_read_back() {
        let directory = sandbox("every");
        for orientation in [
            Orientation::Normal,
            Orientation::FlipHorizontal,
            Orientation::Rotate180,
            Orientation::FlipVertical,
            Orientation::Transpose,
            Orientation::Rotate90,
            Orientation::Transverse,
            Orientation::Rotate270,
        ] {
            let path = directory.join(format!("{}.jpg", orientation.to_exif()));
            a_jpeg(&path);
            save_orientation(&path, orientation).expect("the turn to be saved");

            assert_eq!(
                orientation_of(&path).map(Orientation::from_exif),
                Some(orientation),
                "{orientation:?} did not come back as itself",
            );
        }
    }

    /// A turn saved over a turn replaces it rather than composing with it.
    ///
    /// The file holds what the picture *is*, not a history of what was done to
    /// it, and a viewer that added each turn to the last would send a
    /// photograph somewhere unexpected on the second press.
    #[test]
    fn saving_twice_leaves_the_second_turn_not_both() {
        let path = sandbox("twice").join("picture.jpg");
        a_jpeg(&path);

        save_orientation(&path, Orientation::Rotate90).expect("the first turn");
        save_orientation(&path, Orientation::Rotate180).expect("the second turn");

        assert_eq!(orientation_of(&path), Some(3), "the second turn did not replace the first");
    }
}
