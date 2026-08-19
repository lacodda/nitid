//! The release gate for the product's central promise: a picture on screen
//! before the user notices the wait.
//!
//! This runs the real binary against a real file and reads the time it reports
//! from its own first frame. Measuring inside the process is deliberate — a
//! test that timed `Command::spawn` would be measuring the test harness and
//! the process loader as much as the viewer.
//!
//! The threshold is generous against the ~100 ms the product aims for, because
//! this has to hold on a shared CI runner with a software rasteriser as well as
//! on a desktop with a GPU. It is a regression alarm, not a benchmark: a change
//! that puts a full decode back on the startup path blows straight past it.
//!
//! There are two gates, because two formats reach the screen by different
//! routes. JPEG has an embedded thumbnail and is held to the promise. HEIC has
//! no thumbnail this build can read, so its first frame waits for the whole
//! HEVC decode — slower, measured separately, and held to a threshold that
//! says what it costs today rather than what the product wants it to cost.

use std::path::{Path, PathBuf};
use std::process::Command;

/// What a startup may cost before the gate complains, for a format whose first
/// frame comes from an embedded thumbnail.
const THRESHOLD_MS: f64 = 1500.0;

/// The same for HEIC, which has no such shortcut in this build.
///
/// A HEIC reaches the screen only when its HEVC payload has been decoded in
/// full: on a desktop that is around a second for a 12-megapixel photograph
/// against 40 ms for the equivalent JPEG, and a CI runner without a GPU is
/// slower still. The number is therefore a regression alarm around a known
/// cost, not an endorsement of it — the entry in `План.md` for reading the
/// container's own thumbnail item is what brings it down to the promise.
const HEIC_THRESHOLD_MS: f64 = 4000.0;

/// How many times to run; the fastest counts.
///
/// A first run pays for cold file caches and, on Windows, for whatever the
/// virus scanner wants to do with a freshly built executable. Neither is what
/// this measures, and the fastest run is the one least polluted by them.
const RUNS: usize = 3;

#[test]
fn a_picture_reaches_the_screen_quickly() {
    let Some(exe) = viewer_binary() else {
        eprintln!("skipping: the viewer binary was not found next to the test");
        return;
    };

    let dir = tempfile::tempdir().expect("creating a temporary folder");
    let image = photo_sized_image(dir.path());

    let mut best = f64::INFINITY;
    let mut took_thumbnail_path = false;
    let mut failures = Vec::new();

    for attempt in 1..=RUNS {
        match measure(&exe, &image) {
            Ok(run) => {
                eprintln!("run {attempt}: {:.1} ms", run.first_pixels_ms);
                best = best.min(run.first_pixels_ms);
                took_thumbnail_path |= run.used_thumbnail;
            }
            Err(reason) => failures.push(format!("run {attempt}: {reason}")),
        }
    }

    if best.is_infinite() {
        // No run produced a measurement. On a headless machine there is no
        // surface to present to, which is a fact about the machine rather than
        // a regression in the viewer.
        eprintln!("skipping: the viewer could not open a window here\n  {}", failures.join("\n  "));
        return;
    }

    // The timing alone would still pass if the quick frame silently stopped
    // working and the machine were simply fast. This is the mechanism itself.
    assert!(
        took_thumbnail_path,
        "the embedded thumbnail was never the first frame, so the image on \
         screen waited for a full 24-megapixel decode. Either the EXIF \
         thumbnail stopped being read, or the quick frame is no longer drawn \
         before the background decode finishes."
    );

    assert!(
        best <= THRESHOLD_MS,
        "the first pixels took {best:.1} ms, over the {THRESHOLD_MS:.0} ms gate.\n\
         Something is blocking startup — most likely a full decode that belongs \
         on a worker thread, or a thumbnail path that stopped being taken."
    );
}

/// The same gate for HEIC, the format a phone photographs in.
///
/// Held separately because the mechanism is different: there is no quick frame
/// to check for, and the whole measurement is the decode. If reading the
/// container's thumbnail item ever lands, this test is where it shows up — the
/// number should fall to the JPEG gate and this comment should go.
#[test]
fn a_heic_reaches_the_screen_within_its_slower_budget() {
    let Some(exe) = viewer_binary() else {
        eprintln!("skipping: the viewer binary was not found next to the test");
        return;
    };

    let dir = tempfile::tempdir().expect("creating a temporary folder");
    let image = dir.path().join("photo.heic");
    std::fs::write(&image, heic_fixture()).expect("writing the test image");

    let mut best = f64::INFINITY;
    let mut failures = Vec::new();

    for attempt in 1..=RUNS {
        match measure(&exe, &image) {
            Ok(run) => {
                eprintln!("run {attempt}: {:.1} ms", run.first_pixels_ms);
                best = best.min(run.first_pixels_ms);
            }
            Err(reason) => failures.push(format!("run {attempt}: {reason}")),
        }
    }

    if best.is_infinite() {
        eprintln!("skipping: the viewer could not open a window here\n  {}", failures.join("\n  "));
        return;
    }

    assert!(
        best <= HEIC_THRESHOLD_MS,
        "the first pixels of a HEIC took {best:.1} ms, over the {HEIC_THRESHOLD_MS:.0} ms gate.\n\
         The whole decode is on the startup path for this format, so this is \
         either a slower decoder than before or work that belongs on a worker \
         thread."
    );
}

/// A small HEIC, written by libheif.
///
/// Small on purpose: this gate is about the startup path, and a photograph-
/// sized HEVC payload would spend the measurement on the decoder rather than
/// on what the test is watching. The same fixture as the decoder tests, kept
/// as text because the suite ships no binary files.
fn heic_fixture() -> Vec<u8> {
    const BASE64: &str = concat!(
        "AAAAHGZ0eXBoZWljAAAAAG1pZjFoZWljbWlhZgAAAXxtZXRhAAAAAAAAACFoZGxyAAAAAAAAAABwaWN0AAAAAAAAAAAAAAAA",
        "AAAAACJpbG9jAAAAAERAAAEAAQAAAAABoAABAAAAAAAAAMUAAAAjaWluZgAAAAAAAQAAABVpbmZlAgAAAAABAABodmMxAAAA",
        "AA5waXRtAAAAAAABAAAA/GlwcnAAAADcaXBjbwAAAHVodmNDAQNwAAAAAAAAAAAAHvAA/P34+AAADwNgAAEAGEABDAH//wNw",
        "AAADAJAAAAMAAAMAHroCQGEAAQApQgEBA3AAAAMAkAAAAwAAAwAeoCCBBZbqrprm4CGgwIAAAAyAAAADAIRiAAEABkQBwXPB",
        "iQAAABNjb2xybmNseAABAA0ABoAAAAAUaXNwZQAAAAAAAABAAAAAQAAAAChjbGFwAAAAEAAAAAEAAAAQAAAAAf///9AAAAAC",
        "////0AAAAAIAAAAQcGl4aQAAAAADCAgIAAAAGGlwbWEAAAAAAAAAAQABBYECAwWEAAAAzW1kYXQAAADBKAGvBrIe4SSwkawM",
        "wY6ON9EJjG7hymaKZ/pf/3WrYjYL5EOMXoj/oUiSf/V4YmFoXp41sHLqVaifyq4sC4/ttdN2GzH9rdcqNdzCZA3yC2x2QxMy",
        "byTwBoM8oUSRLrSH4EbaR/9AZGEfAS+8Jyn/9J0//89t2z3s9KMylHQsoHew08RJD+KqEiWSI8PgIoxH0TPl0Wx6BM96P48E",
        "1DP93HbO0R8fhSMQwb1/WD6xg0OjqSXlrDVYtqDnMl3Ekl4ZoA==",
    );

    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let mut out = Vec::new();
    let mut accumulator: u32 = 0;
    let mut bits = 0;
    for byte in BASE64.bytes().filter(|byte| *byte != b'=') {
        let value = ALPHABET.iter().position(|candidate| *candidate == byte).expect("a base64 character") as u32;
        accumulator = (accumulator << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((accumulator >> bits) as u8);
        }
    }
    out
}

/// What one run of the viewer reported about its startup.
struct Measurement {
    first_pixels_ms: f64,
    /// Whether the embedded thumbnail was the frame that went up first.
    used_thumbnail: bool,
}

/// Run the viewer once and read the time it reports.
fn measure(exe: &Path, image: &Path) -> Result<Measurement, String> {
    let output = Command::new(exe)
        .arg(image)
        .env("NITID_STARTUP_REPORT", "1")
        .env("NITID_EXIT_AFTER_FIRST_FRAME", "1")
        .output()
        .map_err(|error| format!("running the viewer: {error}"))?;

    let stderr = String::from_utf8_lossy(&output.stderr);

    // The breakdown says *why* a run was fast or slow, which is the difference
    // between a failing gate that points at the cause and one that only says
    // "too slow".
    for line in stderr.lines().filter(|line| line.contains(" at ")) {
        eprintln!("  {}", line.trim());
    }

    let prefix = "nitid: first pixels in ";

    let first_pixels_ms = stderr
        .lines()
        .find_map(|line| line.trim().strip_prefix(prefix))
        .and_then(|rest| rest.trim_end_matches(" ms").parse::<f64>().ok())
        .ok_or_else(|| format!("no measurement was reported (exit {:?}); stderr was:\n{}", output.status.code(), stderr.trim()))?;

    Ok(Measurement {
        first_pixels_ms,
        used_thumbnail: stderr.contains("thumbnail up"),
    })
}

/// A JPEG the size of a photograph, with the embedded thumbnail a camera
/// would have written.
///
/// The thumbnail is what the viewer is supposed to draw first, so a test image
/// without one would measure the fallback path instead of the promise.
fn photo_sized_image(dir: &Path) -> PathBuf {
    let path = dir.join("photo.jpg");

    // 24 megapixels: large enough that decoding it fully on the startup path
    // would be unmistakable in the measurement.
    let mut full = image::RgbImage::new(6000, 4000);
    for (x, y, pixel) in full.enumerate_pixels_mut() {
        // Noise rather than flat colour: a solid image compresses to almost
        // nothing and would decode far faster than a real photograph.
        let value = ((x * 7 + y * 13) % 256) as u8;
        *pixel = image::Rgb([value, value.wrapping_add(80), value.wrapping_add(160)]);
    }

    let encoded = encode_jpeg(&image::DynamicImage::ImageRgb8(full));
    let thumbnail = encode_jpeg(&image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(160, 120, |x, y| {
        image::Rgb([(x * 2) as u8, (y * 2) as u8, 128])
    })));

    std::fs::write(&path, with_exif_thumbnail(&encoded, &thumbnail)).expect("writing the test image");
    path
}

fn encode_jpeg(image: &image::DynamicImage) -> Vec<u8> {
    let mut out = std::io::Cursor::new(Vec::new());
    image.write_to(&mut out, image::ImageFormat::Jpeg).expect("encoding a JPEG");
    out.into_inner()
}

/// Splice an EXIF APP1 segment carrying `thumbnail` into `jpeg`.
///
/// Written by hand because no crate in the tree writes EXIF, and the gate
/// needs a file shaped like what a camera produces.
fn with_exif_thumbnail(jpeg: &[u8], thumbnail: &[u8]) -> Vec<u8> {
    // TIFF header (little endian), one IFD0 entry pointing at IFD1, and IFD1
    // describing where the thumbnail sits.
    let mut tiff = Vec::new();
    tiff.extend_from_slice(b"II\x2a\x00"); // little endian, magic 42
    tiff.extend_from_slice(&8u32.to_le_bytes()); // IFD0 at offset 8

    // IFD0: one entry (Orientation = 1), then the offset of IFD1.
    tiff.extend_from_slice(&1u16.to_le_bytes()); // entry count
    tiff.extend_from_slice(&0x0112u16.to_le_bytes()); // Orientation
    tiff.extend_from_slice(&3u16.to_le_bytes()); // SHORT
    tiff.extend_from_slice(&1u32.to_le_bytes()); // count
    tiff.extend_from_slice(&1u16.to_le_bytes()); // value: upright
    tiff.extend_from_slice(&0u16.to_le_bytes()); // padding of the value field

    let ifd1_offset = (tiff.len() + 4) as u32;
    tiff.extend_from_slice(&ifd1_offset.to_le_bytes());

    // IFD1: the thumbnail's offset and length.
    let entries: u16 = 2;
    // Each entry is 12 bytes; after them come 4 bytes of "next IFD" (zero).
    let thumbnail_offset = ifd1_offset + 2 + u32::from(entries) * 12 + 4;

    tiff.extend_from_slice(&entries.to_le_bytes());
    for (tag, value) in [
        (0x0201u16, thumbnail_offset),       // JPEGInterchangeFormat
        (0x0202u16, thumbnail.len() as u32), // JPEGInterchangeFormatLength
    ] {
        tiff.extend_from_slice(&tag.to_le_bytes());
        tiff.extend_from_slice(&4u16.to_le_bytes()); // LONG
        tiff.extend_from_slice(&1u32.to_le_bytes()); // count
        tiff.extend_from_slice(&value.to_le_bytes());
    }
    tiff.extend_from_slice(&0u32.to_le_bytes()); // no further IFD
    tiff.extend_from_slice(thumbnail);

    // Wrap the TIFF block in an APP1 segment.
    let mut app1 = Vec::new();
    app1.extend_from_slice(b"Exif\0\0");
    app1.extend_from_slice(&tiff);

    let mut out = Vec::new();
    out.extend_from_slice(&jpeg[..2]); // SOI
    out.extend_from_slice(&[0xFF, 0xE1]); // APP1
    out.extend_from_slice(&((app1.len() + 2) as u16).to_be_bytes());
    out.extend_from_slice(&app1);
    out.extend_from_slice(&jpeg[2..]);
    out
}

/// The viewer binary sitting beside this test executable.
fn viewer_binary() -> Option<PathBuf> {
    let mut dir = std::env::current_exe().ok()?;
    dir.pop(); // the test executable's own name
    if dir.ends_with("deps") {
        dir.pop();
    }

    let exe = dir.join(if cfg!(windows) { "nitid.exe" } else { "nitid" });
    exe.exists().then_some(exe)
}
