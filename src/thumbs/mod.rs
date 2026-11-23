//! Thumbnails for Explorer, of the pictures Windows cannot draw.
//!
//! Three processes, so that nothing that reads a stranger's file is anywhere
//! near Explorer:
//!
//! 1. **The DLL** (`provider`), in the host Explorer keeps for handlers. It
//!    passes the file's bytes on and makes a bitmap of the answer.
//! 2. **`nitid.exe`** ([`command`]), which the DLL starts. It sets the sandbox
//!    up, hands the bytes over unread and says what came back.
//! 3. **The decoder** ([`make`]), in the sandbox every heavy decode runs in
//!    (`sandbox`): no files, no network, a limit on its memory and its time.
//!    The only one of the three that looks at the picture.
//!
//! The middle one is not a formality. A confined process has to be made by one
//! that is not, and the launch is three hundred lines that must be right; they
//! stay in the program, where the tests are, and the DLL stays something that
//! can be read in one sitting.
//!
//! See `docs/adr/0049-explorers-thumbnails-come-from-a-thin-dll.md`.

#[cfg(windows)]
pub mod samples;
#[cfg(windows)]
pub mod shell;
pub mod wire;

// The DLL is compiled by `build.rs`, where clippy does not look. Here it does.
#[cfg(all(windows, test))]
#[allow(dead_code)]
mod provider;

use std::ffi::OsString;
use std::io::{self, Read};

use anyhow::{Context, Result, ensure};

use crate::export;
use crate::format::Format;
use crate::image_source::{self, DecodedImage, Depth, LoadedImage};
use crate::vector::VectorImage;

/// The longest side asked for, from the command line. Held to what a thumbnail
/// can be; `None` for what is not a size.
pub fn side(argument: Option<OsString>) -> Option<u32> {
    let side: u32 = argument?.to_str()?.parse().ok()?;
    (side > 0).then(|| side.min(wire::LONGEST))
}

/// Answer the DLL: the picture on standard input, its thumbnail on standard
/// output. Nothing is written for a picture that has none.
pub fn command(side: u32) -> Result<()> {
    let mut bytes = Vec::new();
    io::stdin()
        .take(wire::LARGEST_FILE + 1)
        .read_to_end(&mut bytes)
        .context("reading the picture")?;
    ensure!(bytes.len() as u64 <= wire::LARGEST_FILE, "the picture is too large for a thumbnail");

    let small = crate::sandbox::thumbnail(&bytes, side)?;
    let picture = wire::Picture {
        width: small.width,
        height: small.height,
        pixels: for_a_bitmap(&small.pixels),
    };
    wire::write(io::stdout().lock(), &picture).context("answering")
}

/// Straight RGBA as a Windows bitmap with an alpha channel holds it: blue
/// first, and every colour multiplied by its alpha.
fn for_a_bitmap(rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgba.len());
    for [red, green, blue, alpha] in rgba.as_chunks::<4>().0 {
        let times = |value: u8| ((u16::from(value) * u16::from(*alpha) + 127) / 255) as u8;
        out.extend_from_slice(&[times(*blue), times(*green), times(*red), *alpha]);
    }
    out
}

/// Make a thumbnail: upright, in sRGB, at eight bits, no longer than `side`.
///
/// Runs in the decoder's sandbox. sRGB because Explorer manages no colour and
/// shows the numbers it is given, so this is the one place a wide-gamut or an
/// HDR picture can be made to look like itself there.
pub fn make(bytes: &[u8], side: u32) -> Result<DecodedImage> {
    // Shapes have no size of their own, and are drawn at the size asked for:
    // a sixteen-pixel icon is as sharp as a poster.
    if Format::detect(bytes).is_some_and(Format::is_vector) {
        let vector = VectorImage::parse(bytes)?;
        let (width, height) = filling(vector.intrinsic_size(), side);
        return vector.rasterise(width, height);
    }

    // The camera's own thumbnail first, as on the viewer's first frame, when
    // it is as large as what was asked for: decoding twelve megapixels to
    // arrive at the same picture would be time thrown away.
    let preview = image_source::decode_thumbnail(bytes);
    if let Some(preview) = &preview
        && preview.image.width.max(preview.image.height) >= side
    {
        return small(preview, side);
    }

    match image_source::decode_here(bytes) {
        Ok(loaded) => small(&loaded, side),
        // A file cut short, say: its thumbnail is at the front and still says
        // which picture it was.
        Err(error) => match &preview {
            Some(preview) => small(preview, side),
            None => Err(error),
        },
    }
}

/// `size`'s proportions with the longer side at `side`, whichever way that
/// scales it.
fn filling(size: (u32, u32), side: u32) -> (u32, u32) {
    let longest = u64::from(size.0.max(size.1).max(1));
    let scaled = |length: u32| ((u64::from(length) * u64::from(side) / longest) as u32).max(1);
    (scaled(size.0), scaled(size.1))
}

/// A decoded picture made small, and then turned and converted.
///
/// In that order because the conversion is the dear part: done to the whole of
/// a forty-megapixel HDR picture it would take longer than decoding it.
fn small(loaded: &LoadedImage, side: u32) -> Result<DecodedImage> {
    let reduced = reduced(&loaded.image, side);
    let image = reduced.as_ref().unwrap_or(&loaded.image);
    export::upright_srgb(image, loaded.profile.as_ref(), loaded.orientation, 0)
}

/// `image` with its longer side at `side`, each pixel the average of the ones
/// it stands for, at the depth it was decoded at. `None` if it is no longer
/// than that already: a thumbnail is never an enlargement.
///
/// Weighted by the alpha. The colour under a transparent pixel is whatever the
/// encoder left there, usually black, and counted like the rest it would draw
/// a dark line around every cut-out.
fn reduced(image: &DecodedImage, side: u32) -> Option<DecodedImage> {
    if image.width.max(image.height) <= side {
        return None;
    }
    let (width, height) = filling((image.width, image.height), side);
    let sample_bytes = image.depth.bytes() as usize;
    let mut pixels = Vec::with_capacity(width as usize * height as usize * 4 * sample_bytes);

    let read = |pixel: usize, channel: usize| -> u64 {
        let at = (pixel * 4 + channel) * sample_bytes;
        match image.depth {
            Depth::Eight => image.pixels.get(at).copied().map(u64::from).unwrap_or_default(),
            Depth::Sixteen => match image.pixels.get(at..at + 2) {
                Some(pair) => u64::from(u16::from_ne_bytes([pair[0], pair[1]])),
                None => 0,
            },
        }
    };
    // The source rows or columns one of the small ones stands for: every one
    // in exactly one, and never none.
    let span = |index: u32, small: u32, large: u32| {
        let from = u64::from(index) * u64::from(large) / u64::from(small);
        let to = u64::from(index + 1) * u64::from(large) / u64::from(small);
        from..to.max(from + 1).min(u64::from(large))
    };

    for row in 0..height {
        let rows = span(row, height, image.height);
        for column in 0..width {
            let columns = span(column, width, image.width);
            let (mut colour, mut alpha, mut count) = ([0u64; 3], 0u64, 0u64);
            for y in rows.clone() {
                for x in columns.clone() {
                    let pixel = (y * u64::from(image.width) + x) as usize;
                    let covered = read(pixel, 3);
                    for (channel, sum) in colour.iter_mut().enumerate() {
                        *sum += read(pixel, channel) * covered;
                    }
                    alpha += covered;
                    count += 1;
                }
            }
            let mut put = |value: u64| match image.depth {
                Depth::Eight => pixels.push(value as u8),
                Depth::Sixteen => pixels.extend_from_slice(&(value as u16).to_ne_bytes()),
            };
            for sum in colour {
                put((sum + alpha / 2).checked_div(alpha).unwrap_or_default());
            }
            put((alpha + count / 2) / count.max(1));
        }
    }

    Some(DecodedImage {
        width,
        height,
        pixels,
        depth: image.depth,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32, pixel: impl Fn(u32, u32) -> [u8; 4]) -> Vec<u8> {
        let image = image::RgbaImage::from_fn(width, height, |x, y| image::Rgba(pixel(x, y)));
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image).write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    fn pixel(image: &DecodedImage, x: u32, y: u32) -> [u8; 4] {
        let at = ((y * image.width + x) * 4) as usize;
        image.pixels[at..at + 4].try_into().unwrap()
    }

    /// The longest side is the one asked for and the proportions are the
    /// picture's, whichever way it lies.
    #[test]
    fn a_thumbnail_is_as_long_as_it_was_asked_to_be() {
        let wide = make(&png(1200, 300, |_, _| [9, 9, 9, 255]), 256).unwrap();
        assert_eq!((wide.width, wide.height), (256, 64));
        let tall = make(&png(300, 1200, |_, _| [9, 9, 9, 255]), 256).unwrap();
        assert_eq!((tall.width, tall.height), (64, 256));
        // A panorama keeps a row of its own rather than rounding away.
        let thread = make(&png(4000, 2, |_, _| [9, 9, 9, 255]), 96).unwrap();
        assert_eq!((thread.width, thread.height), (96, 1));
    }

    /// Never an enlargement: Explorer frames a small picture as a small one.
    #[test]
    fn a_small_picture_stays_its_own_size() {
        let small = make(&png(20, 10, |_, _| [1, 2, 3, 255]), 256).unwrap();
        assert_eq!((small.width, small.height), (20, 10));
        assert_eq!(pixel(&small, 5, 5), [1, 2, 3, 255]);
    }

    /// Shapes are drawn at the size asked for, up as well as down.
    #[test]
    fn shapes_are_drawn_at_the_size_asked_for() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="8"><rect width="16" height="8" fill="#0AC85A"/></svg>"##;
        let drawn = make(svg, 256).unwrap();
        assert_eq!((drawn.width, drawn.height), (256, 128));
        assert_eq!(pixel(&drawn, 128, 64), [0x0A, 0xC8, 0x5A, 0xFF]);
    }

    fn jpeg(width: u32, height: u32, colour: [u8; 3]) -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(width, height, image::Rgb(colour)))
            .write_to(&mut out, image::ImageFormat::Jpeg)
            .unwrap();
        out.into_inner()
    }

    /// `picture` with `thumbnail` in its EXIF, turned a quarter clockwise: what
    /// a camera held on its side writes.
    fn as_a_camera_writes_it(picture: &[u8], thumbnail: &[u8]) -> Vec<u8> {
        let mut tiff = b"II\x2a\x00\x08\x00\x00\x00".to_vec();
        let entry = |tiff: &mut Vec<u8>, tag: u16, kind: u16, value: u32| {
            tiff.extend_from_slice(&tag.to_le_bytes());
            tiff.extend_from_slice(&kind.to_le_bytes());
            tiff.extend_from_slice(&1u32.to_le_bytes());
            tiff.extend_from_slice(&value.to_le_bytes());
        };
        // The first directory: the orientation, and where the second is.
        tiff.extend_from_slice(&1u16.to_le_bytes());
        entry(&mut tiff, 0x0112, 3, 6);
        let second = tiff.len() as u32 + 4;
        tiff.extend_from_slice(&second.to_le_bytes());
        // The second: where the thumbnail is and how long.
        tiff.extend_from_slice(&2u16.to_le_bytes());
        entry(&mut tiff, 0x0201, 4, second + 2 + 2 * 12 + 4);
        entry(&mut tiff, 0x0202, 4, thumbnail.len() as u32);
        tiff.extend_from_slice(&0u32.to_le_bytes());
        tiff.extend_from_slice(thumbnail);

        let mut out = picture[..2].to_vec();
        out.extend_from_slice(&[0xFF, 0xE1]);
        out.extend_from_slice(&((tiff.len() + 8) as u16).to_be_bytes());
        out.extend_from_slice(b"Exif\0\0");
        out.extend_from_slice(&tiff);
        out.extend_from_slice(&picture[2..]);
        out
    }

    /// The camera's own thumbnail is used when it is as large as what was asked
    /// for, the picture when it is not, and either is turned.
    #[test]
    fn the_cameras_thumbnail_comes_first() {
        let photograph = as_a_camera_writes_it(&jpeg(800, 600, [200, 0, 0]), &jpeg(160, 120, [0, 0, 200]));

        let small = make(&photograph, 96).unwrap();
        assert_eq!((small.width, small.height), (72, 96), "it was not turned");
        assert!(
            pixel(&small, 36, 48)[2] > 150,
            "the whole picture was decoded for a thumbnail the camera had made"
        );

        let large = make(&photograph, 256).unwrap();
        assert_eq!((large.width, large.height), (192, 256), "it was not turned");
        assert!(pixel(&large, 96, 128)[0] > 150, "a thumbnail smaller than what was asked for was used");
    }

    /// A picture that will not decode still has the thumbnail in front of it.
    #[test]
    fn a_broken_picture_keeps_the_cameras_thumbnail() {
        let broken = [&jpeg(800, 600, [200, 0, 0])[..2], b"and then nothing a decoder knows".as_slice(), &[0; 64]].concat();
        let photograph = as_a_camera_writes_it(&broken, &jpeg(160, 120, [0, 0, 200]));
        assert!(image_source::decode_here(&photograph).is_err(), "the picture decoded after all");
        let small = make(&photograph, 256).unwrap();
        assert_eq!((small.width, small.height), (120, 160));
        assert!(pixel(&small, 60, 80)[2] > 150);
    }

    /// Explorer shows the numbers it is given, so a wide-gamut picture is put
    /// in sRGB rather than handed over in its own.
    #[test]
    fn the_colour_is_put_in_srgb() {
        let display_p3 = moxcms::ColorProfile::new_display_p3();
        let loaded = LoadedImage {
            image: DecodedImage {
                width: 1,
                height: 1,
                pixels: vec![200, 100, 100, 255],
                depth: Depth::Eight,
            },
            profile: Some(display_p3.clone()),
            ..image_source::decode_here(&png(1, 1, |_, _| [0, 0, 0, 255])).unwrap()
        };
        let made = small(&loaded, 64).unwrap();
        let meant = export::baked(&loaded.image, Some(&display_p3)).pixels;
        assert_eq!(made.pixels, meant);
        assert_ne!(made.pixels, loaded.image.pixels, "the numbers were handed over as they were");
    }

    /// Fine detail averages rather than turning into noise.
    #[test]
    fn detail_is_averaged() {
        let checks = make(&png(512, 512, |x, y| if (x + y) % 2 == 0 { [255, 255, 255, 255] } else { [0, 0, 0, 255] }), 64).unwrap();
        for y in 0..64 {
            for x in 0..64 {
                let [red, ..] = pixel(&checks, x, y);
                assert!((127..=128).contains(&red), "{x},{y} is {red}");
            }
        }
    }

    /// A cut-out's edge is its own colour thinned, not darkened by the black
    /// that lies under everything transparent.
    #[test]
    fn what_is_transparent_lends_no_colour() {
        let image = DecodedImage {
            width: 2,
            height: 1,
            pixels: vec![200, 100, 50, 255, 0, 0, 0, 0],
            depth: Depth::Eight,
        };
        let one = reduced(&image, 1).unwrap();
        assert_eq!(one.pixels, [200, 100, 50, 128]);
        // And nothing at all is nothing, rather than a division by it.
        let nothing = DecodedImage { pixels: vec![0; 8], ..image };
        assert_eq!(reduced(&nothing, 1).unwrap().pixels, [0; 4]);
    }

    /// Sixteen bits are averaged as sixteen: narrowed first, an HDR ramp would
    /// step.
    #[test]
    fn a_deep_picture_is_averaged_at_its_own_depth() {
        let samples: [u16; 8] = [1000, 2000, 3000, 65535, 1001, 2001, 3001, 65535];
        let image = DecodedImage {
            width: 2,
            height: 1,
            pixels: samples.iter().flat_map(|sample| sample.to_ne_bytes()).collect(),
            depth: Depth::Sixteen,
        };
        let one = reduced(&image, 1).unwrap();
        assert_eq!(one.depth, Depth::Sixteen);
        let read: Vec<u16> = one.pixels.as_chunks::<2>().0.iter().map(|pair| u16::from_ne_bytes(*pair)).collect();
        assert_eq!(read, [1001, 2001, 3001, 65535]);
    }

    /// A file nothing can be made of is an error, and not a blank thumbnail
    /// Explorer would keep.
    #[test]
    fn what_is_not_a_picture_has_no_thumbnail() {
        assert!(make(b"", 256).is_err());
        assert!(make(b"a text file", 256).is_err());
        let mut broken = png(64, 64, |_, _| [1, 2, 3, 255]);
        broken.truncate(60);
        assert!(make(&broken, 256).is_err());
    }

    /// Blue first and multiplied, which is what an alpha bitmap is.
    #[test]
    fn a_bitmap_holds_blue_first_and_multiplied() {
        assert_eq!(for_a_bitmap(&[255, 100, 0, 255]), [0, 100, 255, 255]);
        assert_eq!(for_a_bitmap(&[255, 100, 0, 128]), [0, 50, 128, 128]);
        assert_eq!(for_a_bitmap(&[255, 100, 0, 0]), [0, 0, 0, 0]);
    }

    #[test]
    fn a_size_is_a_number_and_held_to_the_largest() {
        let side = |text: &str| side(Some(OsString::from(text)));
        assert_eq!(side("96"), Some(96));
        assert_eq!(side("1000000"), Some(wire::LONGEST));
        for wrong in ["", "0", "-1", "big", "256px"] {
            assert_eq!(side(wrong), None, "{wrong}");
        }
        assert_eq!(super::side(None), None);
    }

    /// What one side writes the other reads, and anything else is refused: the
    /// reply is copied into a bitmap made from the size it claims.
    #[test]
    fn a_reply_is_read_only_when_it_is_whole() {
        let picture = wire::Picture {
            width: 3,
            height: 2,
            pixels: (0..24).collect(),
        };
        let mut reply = Vec::new();
        wire::write(&mut reply, &picture).unwrap();
        assert_eq!(wire::read(&reply, 256), Some(picture));

        for cut in 0..reply.len() {
            assert_eq!(wire::read(&reply[..cut], 256), None, "a reply cut at {cut} was read");
        }
        let mut longer = reply.clone();
        longer.push(0);
        assert_eq!(wire::read(&longer, 256), None);
        assert_eq!(wire::read(&reply, 2), None, "larger than it was asked to be");
        // Another version, and what an older program answers with.
        let mut other = reply.clone();
        other[4] ^= 0xFF;
        assert_eq!(wire::read(&other, 256), None);
        assert_eq!(wire::read(b"NTDS and then something", 256), None);
        // A size that would overflow or be nothing.
        for (width, height) in [(0u32, 2u32), (3, 0), (u32::MAX, u32::MAX)] {
            let mut lying = reply.clone();
            lying[6..10].copy_from_slice(&width.to_le_bytes());
            lying[10..14].copy_from_slice(&height.to_le_bytes());
            assert_eq!(wire::read(&lying, u32::MAX), None, "{width}x{height}");
        }
    }

    /// The registry's spelling of the class.
    #[test]
    fn the_class_is_spelt_as_the_registry_spells_one() {
        assert_eq!(wire::class_text(), "{E36F4421-0F9A-496E-993D-C6C773CBD261}");
    }

    /// The two limits on a file are one limit: a picture the DLL passes on has
    /// to be one the decoder will take.
    #[test]
    fn the_largest_file_is_the_decoders() {
        assert_eq!(wire::LARGEST_FILE, crate::sandbox::protocol::MAX_PAYLOAD as u64);
    }
}
