//! The release gate for the desktop's picture: **is what Windows is handed a
//! picture it can show?**
//!
//! Windows reads a handful of formats for the desktop, ignores a photograph's
//! orientation tag and manages no colour there, so nitid hands it a JPEG made
//! for it (see `src/wallpaper`). The unit test beside that code makes one from
//! a PNG; this gate makes one from each kind of file where something could go
//! wrong on the way — a HEIC, which only decodes in the sandboxed child; a
//! photograph turned by its tag; a vector, drawn at the size it is needed at;
//! a sixteen-bit picture — through the very function the desktop is given,
//! and reads the result back with a decoder that is not nitid's.
//!
//! Nothing here touches the desktop or the Task Scheduler.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Once;

use nitid::testing::{Orientation, prepare_wallpaper, save_orientation};

/// The sandboxed decoder is the viewer itself, started again; from a test the
/// running executable is the test, so the viewer is named.
fn with_decoder() {
    static POINTED: Once = Once::new();
    POINTED.call_once(|| {
        let mut dir = std::env::current_exe().expect("the test's own path");
        dir.pop();
        if dir.ends_with("deps") {
            dir.pop();
        }
        let exe = dir.join(if cfg!(windows) { "nitid.exe" } else { "nitid" });
        // SAFETY: set once, before any decode in this process reads it.
        unsafe { std::env::set_var("NITID_DECODER", exe) };
    });
}

fn made(path: &Path, screens: &[(u32, u32)], fill: bool) -> image::DynamicImage {
    with_decoder();
    let bytes = prepare_wallpaper(path, screens, fill).unwrap_or_else(|error| panic!("{}: {error:#}", path.display()));
    assert_eq!(
        image::guess_format(&bytes).expect("a known format"),
        image::ImageFormat::Jpeg,
        "the desktop was not given a JPEG"
    );
    image::load_from_memory(&bytes).expect("a JPEG another decoder can read")
}

fn folder() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let path = dir.path().to_path_buf();
    (dir, path)
}

/// A HEIC — a phone's photograph, which Windows shows only with a store
/// extension — comes out as a JPEG, decoded in the sandboxed child on the way
/// (in-process off Windows, where there is no sandbox yet).
#[test]
fn a_heic_is_made_a_picture_windows_can_show() {
    let (_dir, dir) = folder();
    let heic = dir.join("phone.heic");
    std::fs::write(&heic, common::heic_fixture()).expect("writing the HEIC");

    let picture = made(&heic, &[(1920, 1080)], true);
    assert!(picture.width() > 0 && picture.height() > 0);
}

/// A photograph taken with the phone on its side carries its turn as a tag,
/// which the desktop would ignore: the picture it gets is upright already.
#[test]
fn a_turned_photograph_is_made_upright() {
    let (_dir, dir) = folder();
    let jpeg = dir.join("sideways.jpg");
    // Wide as stored, red on the left half.
    let mut pixels = image::RgbImage::new(60, 40);
    for (x, _, pixel) in pixels.enumerate_pixels_mut() {
        *pixel = image::Rgb(if x < 30 { [230, 20, 20] } else { [20, 20, 230] });
    }
    pixels.save(&jpeg).expect("writing the JPEG");
    save_orientation(&jpeg, Orientation::Rotate90).expect("tagging it as turned");

    let picture = made(&jpeg, &[(4000, 4000)], true);
    assert_eq!((picture.width(), picture.height()), (40, 60), "the desktop's picture is not upright");
    // Turned a quarter clockwise, the stored left half is now on top.
    let top = picture.to_rgb8().get_pixel(20, 5).0;
    assert!(top[0] > 150 && top[2] < 100, "the turn went the wrong way: the top is {top:?}");
}

/// A vector is drawn at the size the display needs, not enlarged from the
/// size its document declares.
#[test]
fn a_vector_is_drawn_at_the_size_the_display_needs() {
    let (_dir, dir) = folder();
    let svg = dir.join("mark.svg");
    std::fs::write(
        &svg,
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="30" height="20"><rect width="30" height="20" fill="#20a040"/></svg>"##,
    )
    .expect("writing the SVG");

    let picture = made(&svg, &[(300, 200)], true);
    assert_eq!(
        (picture.width(), picture.height()),
        (300, 200),
        "the vector was not drawn at the display's size"
    );
    let middle = picture.to_rgb8().get_pixel(150, 100).0;
    assert!((middle[1] as i32 - 0xa0).abs() < 12, "the colour did not survive: {middle:?}");
}

/// Sixteen bits a channel are brought down to eight once, with the colour
/// where it was, and a big picture is made no bigger than the display needs.
#[test]
fn a_sixteen_bit_picture_keeps_its_colour_and_is_made_the_size_needed() {
    let (_dir, dir) = folder();
    let png = dir.join("deep.png");
    let pixels = image::ImageBuffer::<image::Rgb<u16>, _>::from_pixel(800, 400, image::Rgb([0x8000, 0x2000, 0xF000]));
    pixels.save(&png).expect("writing the 16-bit PNG");

    let picture = made(&png, &[(200, 200)], false);
    assert_eq!((picture.width(), picture.height()), (200, 100), "fitting a 2:1 picture to a square display");
    let pixel = picture.to_rgb8().get_pixel(100, 50).0;
    for (got, wanted) in pixel.iter().zip([0x80u8, 0x20, 0xF0]) {
        assert!((*got as i32 - wanted as i32).abs() <= 4, "the colour moved: {pixel:?}");
    }
}
