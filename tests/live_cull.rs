//! The release gate for the mark a cull leaves: **does anything else see it?**
//!
//! The unit tests in `src/cull.rs` write a mark and read it back with this
//! project's own reader. That is a closed circle — a shared misunderstanding
//! of where the tag goes would pass both halves, and the viewer would ship a
//! feature whose entire promise is that *other programs* can see the
//! selection.
//!
//! So this gate asks the one reader that settles it: the Windows shell's own
//! property system, which is what fills Explorer's `Rating` column and the
//! Properties dialog. If it says "4 Stars" for a file nitid marked, the promise
//! holds. If it says "Unrated", the feature does not work however green the
//! unit tests are.
//!
//! Windows-only, because the claim is about Windows. Elsewhere the gate has
//! nothing to ask and says so by not existing.

#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::process::Command;

/// Somewhere to write files that is not the owner's disk proper.
fn sandbox(name: &str) -> PathBuf {
    let directory = std::env::temp_dir().join("nitid-live-cull").join(name);
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("a sandbox to write into");
    directory
}

/// A JPEG built here rather than committed: a fixture from the owner's archive
/// would put a real photograph in a public repository.
fn a_jpeg(path: &Path) {
    let mut pixels = image::RgbImage::new(64, 48);
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
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(std::io::BufWriter::new(file), 90);
    encoder.encode_image(&pixels).expect("an encoded jpeg");
}

/// What the Windows shell says in its `Rating` column for this file.
///
/// Asked through the shell's own `Shell.Application`, which is the same source
/// Explorer draws the column from — not a second reader that might agree with
/// ours by coincidence. Column 19 is `Rating`; the name is checked rather than
/// assumed, so a Windows that numbers its columns differently fails loudly
/// instead of quietly reading something else.
fn rating_according_to_windows(path: &Path) -> String {
    let folder = path.parent().expect("a file in a folder");
    let name = path.file_name().expect("a name").to_string_lossy().into_owned();

    // The output encoding is set first and it is load-bearing: the shell hands
    // these strings back in the console's OEM codepage, which arrives here as
    // mojibake and made the column check fail for a reason that had nothing to
    // do with ratings. Without it the gate reports the wrong fault.
    let script = format!(
        r#"[Console]::OutputEncoding = [Text.Encoding]::UTF8
$shell = New-Object -ComObject Shell.Application
$folder = $shell.Namespace('{}')
$item = $folder.ParseName('{}')
$column = $folder.GetDetailsOf($null, 19)
if ($column -ne 'Rating') {{ Write-Output ('UNEXPECTED COLUMN: ' + $column); exit }}
Write-Output $folder.GetDetailsOf($item, 19)"#,
        folder.display(),
        name,
    );

    let output = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
        .expect("asking the shell for the rating");

    assert!(
        output.status.success(),
        "the shell refused the question: {}",
        String::from_utf8_lossy(&output.stderr),
    );

    let said = String::from_utf8_lossy(&output.stdout).trim().to_string();
    assert!(!said.starts_with("UNEXPECTED COLUMN"), "{said}");
    said
}

/// The promise of the whole stage, held to the reader that settles it.
///
/// A keep has to reach Explorer's star column. Nothing in this project is
/// involved in the answer.
#[test]
fn windows_sees_the_mark_nitid_wrote() {
    let directory = sandbox("seen-by-windows");
    let path = directory.join("photo.jpg");
    a_jpeg(&path);

    assert_eq!(rating_according_to_windows(&path), "Unrated", "the fixture was rated before anything marked it",);

    nitid::testing::mark_kept(&path).expect("the mark goes in");

    let said = rating_according_to_windows(&path);
    assert!(
        said.contains("Star"),
        "Windows does not see the mark: its Rating column says {said:?}. \
         A selection no other program can read is the one thing this feature exists not to be.",
    );
}

/// Taking the mark off has to reach Explorer too, or a cleared frame keeps a
/// star in the one place the person will look at it.
#[test]
fn windows_sees_the_mark_taken_off_again() {
    let directory = sandbox("cleared");
    let path = directory.join("photo.jpg");
    a_jpeg(&path);

    nitid::testing::mark_kept(&path).expect("the mark goes in");
    assert!(
        rating_according_to_windows(&path).contains("Star"),
        "the mark never arrived, so clearing it proves nothing"
    );

    nitid::testing::mark_cleared(&path).expect("the mark comes off");

    assert_eq!(rating_according_to_windows(&path), "Unrated", "the star outlived the mark that put it there",);
}

/// A rejected frame reads as unrated to Windows, which is the honest answer:
/// no other program has a concept of "rejected" to show.
///
/// This is not a formality, and it is the assertion that earned this whole
/// gate its place. The reject went into `RatingPercent` first, on the
/// reasoning that a percentage below any star's threshold would be invisible
/// — and this test failed, because Windows reads its stars out of that very
/// field. Every rejected frame was showing as a one-star favourite: the
/// *opposite* of the failure the feature exists to avoid, and invisible to
/// every test that read the file back with our own reader.
#[test]
fn a_reject_does_not_look_like_a_rating_to_windows() {
    let directory = sandbox("rejected");
    let path = directory.join("photo.jpg");
    a_jpeg(&path);

    nitid::testing::mark_rejected(&path).expect("the mark goes in");

    let said = rating_according_to_windows(&path);
    assert_eq!(said, "Unrated", "a rejected frame is showing as a rating: {said:?}");

    // And it is still a reject to us — "looks unrated to Windows" must not be
    // achieved by simply not writing anything.
    assert!(nitid::testing::is_rejected(&path), "the reject was not recorded at all");
}
