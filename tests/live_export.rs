//! The release gate for the export: a real picture written out, and opened
//! again through the viewer's own front door.
//!
//! The unit tests in `src/export.rs` check the pieces on pictures a few pixels
//! across. This checks the whole path on something with detail in it, which is
//! where the differences live: a shrink that averages nothing on a flat
//! colour, a quality search that costs no bytes, a bake that is the identity
//! because the fixture had no colour to speak of.
//!
//! Everything is read back with `decode_here` rather than with the `image`
//! crate. That is not an accident: the viewer builds `image` without its webp
//! feature, because WebP is decoded by `image-webp` directly — so `image`
//! cannot read what this writes, and a gate that used it would fail on a file
//! that is perfectly good.

use nitid::testing::{decode_here, export_for_test};

/// A picture with structure: gradients, noise and edges.
///
/// Noise matters. A flat colour compresses to nothing at every quality and
/// averages to itself at every scale, so a fixture without it cannot tell a
/// working shrink or quality search from a broken one.
fn detailed(width: u32, height: u32) -> Vec<u8> {
    let mut pixels = image::RgbaImage::new(width, height);
    let mut seed = 0x1234_5678u32;
    for (x, y, pixel) in pixels.enumerate_pixels_mut() {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        let noise = (seed >> 24) as u8 / 4;
        *pixel = image::Rgba([
            ((x * 255) / width.max(1)) as u8,
            ((y * 255) / height.max(1)) as u8,
            noise.wrapping_add(((x ^ y) & 0xC0) as u8),
            255,
        ]);
    }

    let mut out = std::io::Cursor::new(Vec::new());
    pixels.write_to(&mut out, image::ImageFormat::Png).expect("encoding the fixture");
    out.into_inner()
}

/// Every target writes something the viewer opens again, at the same size.
#[test]
fn a_detailed_picture_survives_every_target() {
    let bytes = detailed(640, 400);
    let loaded = decode_here(&bytes).expect("decoding the fixture");

    for extension in ["jpg", "png", "webp"] {
        let written = export_for_test(&loaded, extension, false, 90).unwrap_or_else(|error| panic!("{extension}: {error:#}"));

        let back = decode_here(&written).unwrap_or_else(|error| panic!("the {extension} does not open again: {error:#}"));
        assert_eq!(back.image.width, loaded.image.width, "{extension}: the width changed");
        assert_eq!(back.image.height, loaded.image.height, "{extension}: the height changed");
    }
}

/// The bake, over a picture that actually has a colour to bake.
///
/// An sRGB fixture makes baking the identity, which proves nothing about the
/// path — so this one is tagged Display P3, the space a phone photographs in.
/// Two things then have to be true: the numbers move, and most of them do.
#[test]
fn baking_a_wide_gamut_picture_writes_different_numbers() {
    let bytes = detailed(320, 200);
    let mut loaded = decode_here(&bytes).expect("decoding the fixture");
    loaded.profile = Some(moxcms::ColorProfile::new_display_p3());

    let plain = export_for_test(&loaded, "png", false, 90).expect("the plain one");
    let baked = export_for_test(&loaded, "png", true, 90).expect("the baked one");
    assert_ne!(plain, baked, "baking a Display P3 picture changed nothing at all");

    let plain_back = decode_here(&plain).expect("reading the plain one");
    let baked_back = decode_here(&baked).expect("reading the baked one");

    let moved = plain_back
        .image
        .pixels
        .iter()
        .zip(&baked_back.image.pixels)
        .filter(|(before, after)| before != after)
        .count();
    let share = moved as f64 / plain_back.image.pixels.len() as f64;

    // Measured at 70% on this fixture. The floor is well under that: the point
    // is to catch a bake that did nothing, not to grade the transform.
    assert!(share > 0.4, "only {:.1}% of the samples moved; the bake barely ran", share * 100.0);
}

/// Quality has to cost bytes on a real picture, or the budget search is
/// choosing between files that are all the same size.
#[test]
fn quality_changes_what_a_jpeg_weighs() {
    let bytes = detailed(480, 320);
    let loaded = decode_here(&bytes).expect("decoding the fixture");

    let low = export_for_test(&loaded, "jpg", false, 20).expect("a low-quality JPEG");
    let high = export_for_test(&loaded, "jpg", false, 95).expect("a high-quality JPEG");

    assert!(
        high.len() > low.len() * 2,
        "quality 95 weighed {} bytes against {} at quality 20 — the setting is not reaching the encoder",
        high.len(),
        low.len()
    );

    // Both still open: a small file that nothing can read is not a small file.
    decode_here(&low).expect("the low-quality one opens");
    decode_here(&high).expect("the high-quality one opens");
}
