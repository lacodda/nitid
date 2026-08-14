//! The decoder process, exercised end to end.
//!
//! These run the real `nitid` binary as a child: the point of the boundary is
//! that a decode survives a hostile file, and only a real process can show
//! that. A unit test would have to stand in for the child, which is precisely
//! the part that must not be assumed.

use std::io::Cursor;
use std::path::PathBuf;

/// The binary under test, built by cargo alongside this test.
///
/// `CARGO_BIN_EXE_nitid` is set for integration tests, so the child is the
/// same executable a user runs rather than something arranged for the test.
fn decoder() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_nitid"))
}

/// Point the sandbox at the binary cargo just built.
///
/// Without this the child would be the test harness, which does not answer the
/// protocol — and every assertion below would pass for the wrong reason.
fn with_decoder<T>(body: impl FnOnce() -> T) -> T {
    // SAFETY: the tests in this file run in one process; the variable is set
    // once at the start of each and read by the spawn that follows.
    unsafe { std::env::set_var("NITID_DECODER", decoder()) };
    body()
}

/// A PNG, encoded here so the test ships no binary fixture.
fn png(width: u32, height: u32) -> Vec<u8> {
    let buffer = image::RgbaImage::from_pixel(width, height, image::Rgba([10, 200, 90, 255]));
    let mut out = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(buffer)
        .write_to(&mut out, image::ImageFormat::Png)
        .expect("encoding a synthetic PNG");
    out.into_inner()
}

/// The whole boundary: a real child is launched, confined, handed a file it is
/// never told the path of, and its pixels come back intact.
#[test]
fn a_file_decodes_in_the_sandboxed_process() {
    let decoded = with_decoder(|| nitid::testing::decode_sandboxed(&png(7, 5))).expect("the sandboxed decode should succeed");

    assert_eq!((decoded.width, decoded.height), (7, 5));
    assert_eq!(decoded.pixels.len(), 7 * 5 * 4);
    for pixel in decoded.pixels.chunks_exact(4) {
        assert_eq!(pixel, [10, 200, 90, 255], "the pixels did not survive the crossing");
    }
}

/// A large image crosses the pipe in one piece: the reply is megabytes, which
/// is where a naive read would stop short and hand back half a picture.
#[test]
fn a_large_image_crosses_the_pipe_whole() {
    let decoded = with_decoder(|| nitid::testing::decode_sandboxed(&png(800, 600))).expect("a large sandboxed decode should succeed");

    assert_eq!((decoded.width, decoded.height), (800, 600));
    assert_eq!(decoded.pixels.len(), 800 * 600 * 4);
    assert!(decoded.pixels.chunks_exact(4).all(|pixel| pixel == [10, 200, 90, 255]));
}

/// The reason the boundary exists: a file that defeats the decoder comes back
/// as a message, and the caller lives to show it.
#[test]
fn a_broken_file_is_reported_rather_than_taking_the_viewer_down() {
    with_decoder(|| {
        assert!(nitid::testing::decode_sandboxed(b"this is not an image at all").is_err());
        assert!(nitid::testing::decode_sandboxed(&[]).is_err());

        let mut truncated = png(4, 4);
        truncated.truncate(20);
        assert!(nitid::testing::decode_sandboxed(&truncated).is_err());
    });
}

/// Corruption in the middle of a valid file is the shape a fuzzer finds
/// crashes in. Every one of these must return — an error is fine, a hang or a
/// dead viewer is not.
#[test]
fn corrupted_files_never_take_the_viewer_down() {
    let original = png(32, 32);

    with_decoder(|| {
        for cut in [8usize, 20, 40, 80, 160] {
            let mut broken = original.clone();
            if cut < broken.len() {
                broken.truncate(cut);
                let _ = nitid::testing::decode_sandboxed(&broken);
            }
        }

        for flip in [10usize, 25, 50, 100, 200] {
            let mut broken = original.clone();
            if flip < broken.len() {
                broken[flip] ^= 0xFF;
                let _ = nitid::testing::decode_sandboxed(&broken);
            }
        }
    });
}

/// Several decodes in a row: each child is created and reaped cleanly, so
/// nothing accumulates. A leaked process or handle would show up here first.
#[test]
fn repeated_decodes_do_not_accumulate_processes() {
    with_decoder(|| {
        for _ in 0..8 {
            let decoded = nitid::testing::decode_sandboxed(&png(16, 16)).expect("each decode should succeed");
            assert_eq!(decoded.pixels.len(), 16 * 16 * 4);
        }
    });
}
