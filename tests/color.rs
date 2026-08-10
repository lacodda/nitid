//! The release gate for colour management: a tagged image must actually be
//! converted, and an untagged one must be left alone.
//!
//! The unit tests in `src/color.rs` check the matrices in isolation. This
//! checks the path a real file takes — profile embedded by an encoder, read
//! back out of the bytes on disk, turned into a transform.

use nitid::testing::{ColorTransform, profile_from};

/// A PNG carrying the Display P3 profile, written the way an encoder would.
fn tagged_png(profile: &[u8]) -> Vec<u8> {
    let mut png = std::io::Cursor::new(Vec::new());
    image::RgbaImage::from_pixel(4, 4, image::Rgba([200, 40, 60, 255]))
        .write_to(&mut png, image::ImageFormat::Png)
        .expect("encoding a PNG");
    let png = png.into_inner();

    // Splice an iCCP chunk in after the IHDR, where a real encoder puts it.
    let mut compressed = Vec::new();
    {
        use std::io::Write;
        let mut encoder = flate2::write::ZlibEncoder::new(&mut compressed, flate2::Compression::fast());
        encoder.write_all(profile).expect("compressing the profile");
        encoder.finish().expect("finishing the profile stream");
    }

    let mut chunk = Vec::new();
    chunk.extend_from_slice(b"ICC\0"); // profile name, then a zero
    chunk.push(0); // compression method: deflate
    chunk.extend_from_slice(&compressed);

    let mut out = Vec::new();
    // Signature, then IHDR: 8 + (4 length + 4 type + 13 data + 4 crc).
    let after_ihdr = 8 + 25;
    out.extend_from_slice(&png[..after_ihdr]);
    out.extend_from_slice(&(chunk.len() as u32).to_be_bytes());
    out.extend_from_slice(b"iCCP");
    out.extend_from_slice(&chunk);

    let mut crc = crc32(b"iCCP");
    crc = crc32_continue(crc, &chunk);
    out.extend_from_slice(&crc.to_be_bytes());
    out.extend_from_slice(&png[after_ihdr..]);
    out
}

fn crc32(data: &[u8]) -> u32 {
    crc32_continue(0xFFFF_FFFF, data) ^ 0xFFFF_FFFF ^ 0xFFFF_FFFF
}

fn crc32_continue(mut crc: u32, data: &[u8]) -> u32 {
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    crc
}

#[test]
fn a_tagged_png_is_recognised_and_converted() {
    let p3 = moxcms::ColorProfile::new_display_p3();
    let encoded = p3.encode().expect("serialising Display P3");

    let file = tagged_png(&encoded);
    let profile = profile_from(&file).expect("the iCCP chunk should be found and parsed");

    let transform = ColorTransform::new(&profile, &moxcms::ColorProfile::new_srgb());
    assert!(!transform.is_identity, "a Display P3 image on an sRGB display must be converted");
}

#[test]
fn an_untagged_png_needs_no_conversion() {
    let mut png = std::io::Cursor::new(Vec::new());
    image::RgbaImage::from_pixel(4, 4, image::Rgba([200, 40, 60, 255]))
        .write_to(&mut png, image::ImageFormat::Png)
        .expect("encoding a PNG");

    assert!(
        profile_from(&png.into_inner()).is_none(),
        "an untagged file must report no profile, so the viewer assumes sRGB"
    );
}
