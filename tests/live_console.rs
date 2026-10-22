//! The release gate for the console: `nitid info` and `nitid convert` run as
//! the built program, over files on disk, and what they write read back by the
//! viewer's own front door.
//!
//! The unit tests check the pieces. This checks what a person at a prompt
//! gets: that a pattern Windows did not expand is expanded, that a converted
//! photograph keeps its colour, its account of itself, its mark and its date,
//! that it comes out the right way up, and that a second run touches nothing.

mod common;

use std::path::Path;
use std::process::{Command, Output};
use std::time::{Duration, SystemTime};

use nitid::testing::{ColorTransform, Orientation, decode_here, is_rejected, mark_rejected, read_metadata};

/// Run the console binary with `args`, from `dir`, keeping its files there.
fn nitid(dir: &Path, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_nitid"));
    common::keep_data_in(&mut command, dir);
    command.current_dir(dir).args(args).output().expect("running nitid")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// An EXIF block naming a camera and the phone held sideways.
fn exif_block() -> Vec<u8> {
    let make = b"Synthetic Camera\0";
    let mut tiff = b"II*\0".to_vec();
    tiff.extend_from_slice(&8u32.to_le_bytes());
    tiff.extend_from_slice(&2u16.to_le_bytes());
    // Make: ASCII, its text after the directory.
    tiff.extend_from_slice(&0x010Fu16.to_le_bytes());
    tiff.extend_from_slice(&2u16.to_le_bytes());
    tiff.extend_from_slice(&(make.len() as u32).to_le_bytes());
    tiff.extend_from_slice(&(8u32 + 2 + 2 * 12 + 4).to_le_bytes());
    // Orientation: SHORT 6, a quarter clockwise, held in the entry itself.
    tiff.extend_from_slice(&0x0112u16.to_le_bytes());
    tiff.extend_from_slice(&3u16.to_le_bytes());
    tiff.extend_from_slice(&1u32.to_le_bytes());
    tiff.extend_from_slice(&6u16.to_le_bytes());
    tiff.extend_from_slice(&0u16.to_le_bytes());
    tiff.extend_from_slice(&0u32.to_le_bytes());
    tiff.extend_from_slice(make);
    tiff
}

/// A photograph as a phone leaves it: stored 60 wide and 40 tall with a tag
/// that turns it, in Display P3, with a camera in its EXIF.
fn phone_jpeg() -> Vec<u8> {
    let mut pixels = image::RgbImage::new(60, 40);
    for (x, y, pixel) in pixels.enumerate_pixels_mut() {
        *pixel = image::Rgb([(x * 4) as u8, (y * 6) as u8, 128]);
    }
    let mut out = Vec::new();
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 92);
    image::ImageEncoder::set_icc_profile(&mut encoder, moxcms::ColorProfile::new_display_p3().encode().unwrap()).unwrap();
    image::ImageEncoder::set_exif_metadata(&mut encoder, exif_block()).unwrap();
    encoder.encode_image(&pixels).expect("encoding the fixture");
    out
}

/// A moment well in the past, to tell a kept file time from a new one.
fn long_ago() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_600_000_000)
}

fn set_modified(path: &Path, time: SystemTime) {
    std::fs::File::options().write(true).open(path).unwrap().set_modified(time).unwrap();
}

fn modified(path: &Path) -> SystemTime {
    std::fs::metadata(path).unwrap().modified().unwrap()
}

/// Whether a profile converts to sRGB the way Display P3 does.
fn is_display_p3(profile: Option<&moxcms::ColorProfile>) -> bool {
    let srgb = moxcms::ColorProfile::new_srgb();
    let wanted = ColorTransform::for_image(Some(&moxcms::ColorProfile::new_display_p3()), &srgb).matrix;
    let Some(profile) = profile else {
        return false;
    };
    let got = ColorTransform::for_image(Some(profile), &srgb).matrix;
    got.iter().flatten().zip(wanted.iter().flatten()).all(|(a, b)| (a - b).abs() < 1e-3)
}

/// `info` says what the viewer reads: the size as shown, the turn, the
/// colour and the camera — in text, and in JSON with the same facts as
/// values.
#[test]
fn info_says_what_the_file_says() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("phone.jpg"), phone_jpeg()).unwrap();

    let output = nitid(dir.path(), &["info", "phone.jpg"]);
    assert!(output.status.success(), "{output:?}");
    let text = stdout(&output);
    for line in [
        "phone.jpg",
        "  Size         40 x 60",
        "  Format       JPEG",
        "  Orientation  turned a quarter clockwise (EXIF 6)",
        "  Primaries    Display P3",
        "  Transfer     sRGB curve",
        "  Camera       Synthetic Camera",
    ] {
        assert!(text.lines().any(|candidate| candidate == line), "{line:?} is missing from:\n{text}");
    }

    let output = nitid(dir.path(), &["info", "--json", "*.jpg"]);
    assert!(output.status.success(), "{output:?}");
    let json = stdout(&output);
    for field in [
        "\"path\": \"phone.jpg\"",
        "\"width\": 40",
        "\"height\": 60",
        "\"orientation\": 6",
        "\"primaries\": \"Display P3\"",
        "\"camera\": \"Synthetic Camera\"",
    ] {
        assert!(json.contains(field), "{field} is missing from:\n{json}");
    }
}

/// What names nothing is said, the rest is still described, and the exit
/// code says something went wrong — in JSON as an entry a script can pair
/// with what it asked.
#[test]
fn info_reports_what_it_could_not_read_and_goes_on() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("phone.jpg"), phone_jpeg()).unwrap();
    std::fs::write(dir.path().join("broken.png"), b"\x89PNG\r\n\x1a\nnot really").unwrap();

    let output = nitid(dir.path(), &["info", "--json", "phone.jpg", "gone.jpg", "broken.png"]);
    assert!(!output.status.success(), "a missing and a broken file ended in success");
    let json = stdout(&output);
    assert!(json.contains("\"path\": \"phone.jpg\""), "{json}");
    assert!(json.contains("\"path\": \"gone.jpg\",\n    \"error\": \"no such file or folder\""), "{json}");
    assert!(json.contains("\"path\": \"broken.png\",\n    \"error\": "), "{json}");
}

/// The conversion a phone's pictures are run through, from a pattern no
/// Windows shell expands: the photograph comes out upright with no tag to
/// turn it, in its own colour with the profile written, with its camera,
/// its reject mark and its date — and a second run leaves it alone.
#[test]
fn convert_keeps_the_photograph_and_replaces_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("phone.jpg");
    std::fs::write(&source, phone_jpeg()).unwrap();
    mark_rejected(&source).expect("marking the fixture");
    set_modified(&source, long_ago());

    for (target, written) in [("jpeg", "phone.jpg"), ("png", "phone.png"), ("webp", "phone.webp")] {
        let output = nitid(dir.path(), &["convert", "*.jpg", "--to", target, "--out", "converted"]);
        assert!(output.status.success(), "{target}: {output:?}");
        let path = dir.path().join("converted").join(written);
        assert!(
            stdout(&output).starts_with(&format!("phone.jpg -> {}", Path::new("converted").join(written).display())),
            "{target}: {}",
            stdout(&output)
        );

        let bytes = std::fs::read(&path).unwrap_or_else(|error| panic!("{target}: {error}"));
        let back = decode_here(&bytes).unwrap_or_else(|error| panic!("{target}: {error:#}"));
        assert_eq!((back.image.width, back.image.height), (40, 60), "{target}: not written the way up it is shown");
        assert_eq!(back.orientation, Orientation::Normal, "{target}: the file still carries a turn");
        assert!(is_display_p3(back.profile.as_ref()), "{target}: the profile was not written");
        assert_eq!(
            read_metadata(&bytes).shot.camera.as_deref(),
            Some("Synthetic Camera"),
            "{target}: the EXIF was lost"
        );
        assert_eq!(modified(&path), long_ago(), "{target}: the file's date was not kept");
        if target == "jpeg" {
            assert!(is_rejected(&path), "the reject mark was not carried");
        }
    }

    // A second run finds every name taken and writes nothing.
    let converted = dir.path().join("converted").join("phone.jpg");
    let before = std::fs::read(&converted).unwrap();
    let output = nitid(dir.path(), &["convert", "phone.jpg", "--to", "jpg", "--out", "converted"]);
    assert!(output.status.success(), "{output:?}");
    assert!(stdout(&output).contains("already there, left alone"), "{}", stdout(&output));
    assert_eq!(std::fs::read(&converted).unwrap(), before, "a taken name was written over");
}

/// A HEIC, decoded through the sandboxed decoder the window uses, comes out
/// as a JPEG of the size it is shown at; a folder names its pictures, and the
/// run ends in a count.
#[test]
fn convert_writes_a_heic_as_a_jpeg() {
    let dir = tempfile::tempdir().unwrap();
    let heic = common::heic_fixture();
    std::fs::write(dir.path().join("a.heic"), &heic).unwrap();
    std::fs::write(dir.path().join("b.heic"), &heic).unwrap();
    let shown = decode_here(&heic).expect("the fixture decodes").display_size();

    let output = nitid(dir.path(), &["convert", ".", "--to", "jpeg"]);
    assert!(output.status.success(), "{output:?}");
    assert!(stdout(&output).trim_end().ends_with("converted 2 of 2"), "{}", stdout(&output));
    for name in ["a.jpg", "b.jpg"] {
        let back = decode_here(&std::fs::read(dir.path().join(name)).unwrap()).unwrap_or_else(|error| panic!("{name}: {error:#}"));
        assert_eq!((back.image.width, back.image.height), shown, "{name}");
    }
}
