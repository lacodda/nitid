//! A turn saved to a file is the turn the viewer reads back.
//!
//! The unit tests in `rotate` check the bytes; these check the loop. The
//! orientation is written by one half of the viewer and read by the other —
//! the same path that runs when a picture is opened — so a mismatch between
//! what is written and what is understood shows up here and nowhere else.

use nitid::testing::{Orientation, save_orientation};

/// Somewhere to write that is not the owner's disk proper.
fn sandbox(name: &str) -> std::path::PathBuf {
    let directory = std::env::temp_dir().join("nitid-rotation-tests").join(name);
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("a sandbox to write into");
    directory
}

/// A photograph-shaped JPEG, built here rather than committed.
///
/// Big enough and noisy enough that re-encoding it could not come out the same
/// by accident, which is what makes the byte comparison below mean something.
fn a_photograph(path: &std::path::Path) {
    let mut pixels = image::RgbImage::new(320, 240);
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
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
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(std::io::BufWriter::new(file), 91);
    encoder.encode_image(&pixels).expect("an encoded jpeg");
}

/// The compressed image data, which a save must not touch.
fn scan_data(path: &std::path::Path) -> Vec<u8> {
    let bytes = std::fs::read(path).expect("the file back");
    let start = bytes.windows(2).position(|pair| pair == [0xFF, 0xDA]).expect("a start-of-scan marker");
    bytes[start..].to_vec()
}

/// Save a turn, then open the file the way the viewer does and ask what it
/// says: the write and the read have to agree.
#[test]
fn the_viewer_reads_back_the_turn_it_saved() {
    let directory = sandbox("round-trip");

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
        a_photograph(&path);
        let before = scan_data(&path);

        save_orientation(&path, orientation).expect("the turn to be saved");

        // What the viewer itself makes of the file afterwards.
        let bytes = std::fs::read(&path).expect("the file back");
        let decoded = nitid::testing::decode_here(&bytes).expect("the viewer to open what it just wrote");

        assert_eq!(
            decoded.orientation, orientation,
            "the viewer opened a file it had just written and read {:?} where {orientation:?} was saved",
            decoded.orientation,
        );
        assert_eq!(
            scan_data(&path),
            before,
            "saving {orientation:?} changed the compressed image data, so the picture was re-encoded",
        );
    }
}
