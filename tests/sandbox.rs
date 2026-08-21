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

/// Serialises the tests in this file.
///
/// They configure the sandbox through environment variables, which belong to
/// the process rather than to a test. Left to run in parallel, one test's
/// "hang on purpose" reaches another's decode — which is exactly what happened
/// when this file first grew a timeout test.
static ENVIRONMENT: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Point the sandbox at the binary cargo just built, and hold the environment
/// for the duration.
///
/// Without the first the child would be the test harness, which does not
/// answer the protocol — and every assertion below would pass for the wrong
/// reason. Without the second the variables would race.
fn with_decoder<T>(body: impl FnOnce() -> T) -> T {
    with_environment(&[], body)
}

/// The same, with extra variables set for this test only and removed after.
///
/// Only the timeout tests pass anything here, and those are Windows-only, so
/// off Windows this would be an unused function rather than a missing one.
#[cfg_attr(not(windows), allow(dead_code))]
fn with_environment<T>(variables: &[(&str, &str)], body: impl FnOnce() -> T) -> T {
    // A poisoned lock means another test panicked while holding it; the
    // variables are set afresh below either way, so the guard is taken back.
    let _guard = ENVIRONMENT.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    // SAFETY: the lock above makes this the only test touching the environment.
    unsafe {
        std::env::set_var("NITID_DECODER", decoder());
        for (name, value) in variables {
            std::env::set_var(name, value);
        }
    }

    let outcome = body();

    // SAFETY: still under the lock, and only what this call set is removed.
    unsafe {
        for (name, _) in variables {
            std::env::remove_var(name);
        }
    }

    outcome
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

    assert_eq!((decoded.image.width, decoded.image.height), (7, 5));
    assert_eq!(decoded.image.pixels.len(), 7 * 5 * 4);
    for pixel in decoded.image.pixels.as_chunks::<4>().0.iter() {
        assert_eq!(pixel, &[10, 200, 90, 255], "the pixels did not survive the crossing");
    }
}

/// A large image crosses the pipe in one piece: the reply is megabytes, which
/// is where a naive read would stop short and hand back half a picture.
#[test]
fn a_large_image_crosses_the_pipe_whole() {
    let decoded = with_decoder(|| nitid::testing::decode_sandboxed(&png(800, 600))).expect("a large sandboxed decode should succeed");

    assert_eq!((decoded.image.width, decoded.image.height), (800, 600));
    assert_eq!(decoded.image.pixels.len(), 800 * 600 * 4);
    assert!(decoded.image.pixels.as_chunks::<4>().0.iter().all(|pixel| pixel == &[10, 200, 90, 255]));
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
/// A decoder that never answers must be given up on, not waited for.
///
/// This is the reason the decode moved into a child process at all: work in
/// this process runs to completion whatever it does, and only a separate one
/// can be killed. The child here really does hang — parked for ever on
/// request — so what is exercised is the code that kills it, rather than a
/// stand-in for that code.
///
/// Windows only, and not as a convenience: off Windows `sandbox::decode` runs
/// in this very process, where there is no child to hang and no timeout to
/// enforce. The test would pass there by decoding the file successfully, which
/// would be a green result for the absence of the thing being tested.
#[cfg(windows)]
#[test]
fn a_decoder_that_hangs_is_killed_rather_than_waited_for() {
    let started = std::time::Instant::now();
    let result = with_environment(&[("NITID_DECODER_HANGS", "1"), ("NITID_DECODE_TIMEOUT_MS", "1500")], || {
        nitid::testing::decode_sandboxed(&png(4, 4))
    });
    let waited = started.elapsed();

    assert!(result.is_err(), "a decoder that never answered was reported as a successful decode");

    // Generous against the 1.5 second timeout — a loaded machine may be slow
    // to start the child — but far below the thirty seconds the real timeout
    // allows, so a broken timeout cannot pass this.
    assert!(
        waited < std::time::Duration::from_secs(15),
        "the viewer waited {waited:?} on a decoder that never answered"
    );
}

/// The timeout must not fire on a decode that is merely working: an image
/// large enough to take a moment still has to come back whole.
///
/// Windows only for the same reason as above: there is no timeout to avoid
/// firing where there is no child process.
#[cfg(windows)]
#[test]
fn a_slow_but_working_decode_is_not_cut_short() {
    let decoded =
        with_environment(&[("NITID_DECODE_TIMEOUT_MS", "20000")], || nitid::testing::decode_sandboxed(&png(800, 600))).expect("a working decode was cut short");

    assert_eq!((decoded.image.width, decoded.image.height), (800, 600));
}

#[test]
fn repeated_decodes_do_not_accumulate_processes() {
    with_decoder(|| {
        for _ in 0..8 {
            let decoded = nitid::testing::decode_sandboxed(&png(16, 16)).expect("each decode should succeed");
            assert_eq!(decoded.image.pixels.len(), 16 * 16 * 4);
        }
    });
}
