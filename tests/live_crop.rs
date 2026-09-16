//! The release gate for the crop: real files cropped through the viewer's own
//! path, and opened again through its own front door.
//!
//! The unit tests in `src/jpeg_lossless.rs` check the coder on pictures a
//! hundred pixels across, written by one encoder. This runs the whole path
//! over larger, camera-shaped files at several sizes and subsamplings, which
//! is where the off-by-one lives: a picture whose last MCU is partial, a
//! chroma plane half the size of the luma one, a crop that starts in the
//! middle of the image rather than at its origin.
//!
//! The promise being gated is exact, and it is worth stating precisely
//! because it is not quite "the same pixels".
//!
//! **A lossless crop keeps the file's own coefficients and its quantisation
//! tables.** Nothing is dequantised, nothing is re-encoded, so the picture is
//! not quantised a second time. For a 4:4:4 file that shows up as decoded
//! pixels identical to the original's, to the byte, and the tests below demand
//! exactly that.
//!
//! For a **subsampled** file — 4:2:0, which is what cameras write — the
//! *interior* is identical to the byte and a border one pixel wide is not.
//! Measured, not reasoned: on the committed 4:2:0 fixture an offset crop
//! differs in 412 pixels of 10240, every one of them on the cut edges, none of
//! them interior, by at most 16 of 255. The cause is chroma upsampling. The
//! chroma plane is half resolution, so the decoder interpolates it; a pixel on
//! the cut edge was interpolated in the original against a neighbour that is
//! now outside the picture, and the decoder uses the edge sample instead. The
//! coefficients it interpolates from are the same ones. That is what "no
//! encoder in the path" buys and what it does not.
//!
//! # Why a JPEG is committed here
//!
//! The `image` crate's encoder writes **4:4:4** and has no way to ask for
//! anything else — measured by reading the sampling factors out of a file it
//! wrote, not assumed. So every fixture built in code has one block per
//! component per MCU, and the whole subsampled path — which is what a camera
//! or a phone actually writes, and the reason the grid is 16x16 rather than
//! 8x8 — went untested.
//!
//! It was not untested in a way anyone would notice: the suite was green, and
//! a mutation that made the encoder write one block per component per MCU
//! survived every test in the project. `tests/fixtures/subsampled-420.jpg` is
//! the answer — a synthetic picture encoded by libjpeg through Pillow, so it
//! is both a real 4:2:0 file and a third-party encoding, and it kills that
//! mutation.

use nitid::testing::{Rect, crop_losslessly, cut, decode_here, grid_of, snap};

/// A picture with structure in it, encoded as a JPEG by the `image` crate.
///
/// Noise, and at a quality the default encoder does not use. Both matter: a
/// smooth gradient compresses to so few non-zero coefficients that a crop
/// which dropped the AC terms altogether would still decode to something that
/// looks like the picture.
///
/// Encoded by `image` rather than by anything under test — nothing in this
/// crate writes a JPEG — so the fixture cannot agree with a broken reader by
/// sharing its mistake.
fn a_jpeg(width: u32, height: u32, quality: u8) -> Vec<u8> {
    let mut pixels = image::RgbImage::new(width, height);
    let mut seed = 0x9E37_79B9u32;
    for (x, y, pixel) in pixels.enumerate_pixels_mut() {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        let noise = (seed >> 24) as u8;
        // A gradient with noise on top and a hard edge through it: the three
        // things a JPEG codes differently.
        let edge = if (x / 32 + y / 32) % 2 == 0 { 40u8 } else { 200 };
        *pixel = image::Rgb([((x * 255) / width.max(1)) as u8, noise / 3 + edge / 2, ((y * 255) / height.max(1)) as u8]);
    }

    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(std::io::Cursor::new(&mut out), quality)
        .encode_image(&pixels)
        .expect("an encoded jpeg");
    out
}

/// A real 4:2:0 JPEG, as a camera writes one.
///
/// Committed rather than built because nothing in this crate's dependencies
/// can encode subsampled chroma — see the note at the top of this file. It is
/// synthetic: a gradient with noise and hard edges, never a photograph.
fn a_subsampled_jpeg() -> Vec<u8> {
    std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/subsampled-420.jpg")).expect("the 4:2:0 fixture is missing")
}

/// The picture a file decodes to, through the viewer's own decoder.
fn pixels(bytes: &[u8]) -> (u32, u32, Vec<u8>) {
    let loaded = decode_here(bytes).expect("the viewer opens what it wrote");
    (loaded.image.width, loaded.image.height, loaded.image.pixels.clone())
}

/// Every pixel the crop kept is the pixel the original had there.
fn assert_same_pixels(original: &(u32, u32, Vec<u8>), cropped: &(u32, u32, Vec<u8>), at: (u32, u32)) {
    let (source_width, _, before) = original;
    let (width, height, after) = cropped;

    for row in 0..*height {
        for column in 0..*width {
            let from = (((row + at.1) * source_width + column + at.0) * 4) as usize;
            let to = ((row * width + column) * 4) as usize;
            assert_eq!(
                after[to..to + 4],
                before[from..from + 4],
                "the pixel at {column},{row} changed: the crop re-encoded the picture"
            );
        }
    }
}

/// Every pixel away from the cut edges is the pixel the original had there,
/// and the border is within what chroma upsampling can account for.
///
/// Two assertions rather than a loosened one. The interior is where a coder
/// bug shows — a dropped block, a mis-ordered component, a DC difference taken
/// against the wrong neighbour all land in the middle of the picture, and
/// demanding exactness there is what keeps this a gate. The border is held to
/// a bound instead, because the decoder legitimately answers differently there
/// (see the note at the top of this file) — but it is held, because a bound is
/// still a statement, and an unbounded border would hide a coder that had gone
/// wrong one MCU in.
fn assert_interior_is_identical(original: &(u32, u32, Vec<u8>), cropped: &(u32, u32, Vec<u8>), at: (u32, u32)) {
    let (source_width, _, before) = original;
    let (width, height, after) = cropped;

    // What chroma upsampling can move a sample by at the edge of a picture.
    // Generous enough not to be flaky, far tighter than a re-encode.
    const BORDER_TOLERANCE: i32 = 40;

    for row in 0..*height {
        for column in 0..*width {
            let from = (((row + at.1) * source_width + column + at.0) * 4) as usize;
            let to = ((row * width + column) * 4) as usize;
            let edge = column == 0 || row == 0 || column + 1 == *width || row + 1 == *height;

            if edge {
                for channel in 0..4 {
                    let moved = i32::from(after[to + channel]) - i32::from(before[from + channel]);
                    assert!(
                        moved.abs() <= BORDER_TOLERANCE,
                        "the edge pixel at {column},{row} moved by {moved}, which is more than upsampling explains"
                    );
                }
            } else {
                assert_eq!(
                    after[to..to + 4],
                    before[from..from + 4],
                    "the pixel at {column},{row} changed: the crop re-encoded the picture"
                );
            }
        }
    }
}

/// The whole promise, over a spread of sizes and qualities.
///
/// The sizes are chosen against the grid: one that fills its last MCU exactly,
/// two that do not, in both directions. That last row and column of partial
/// units is where a coder loses a block, and a fixture that always divided
/// evenly could never show it.
#[test]
fn a_lossless_crop_keeps_the_pixels_exactly() {
    for (width, height, quality) in [(320u32, 240u32, 85u8), (300, 200, 92), (255, 171, 78), (512, 384, 60)] {
        let bytes = a_jpeg(width, height, quality);
        let grid = grid_of(&bytes).unwrap_or_else(|| panic!("{width}x{height} at q{quality} is not a baseline jpeg"));
        let original = pixels(&bytes);

        // A rectangle inside the picture, on the grid, not at the origin: the
        // origin is the case that works even when the offset is ignored.
        let wanted = Rect::new(grid.width, grid.height, width - grid.width * 2, height - grid.height * 2);
        let rect = snap(wanted, (width, height), grid).expect("a rectangle inside the picture");

        let cropped = crop_losslessly(&bytes, rect)
            .unwrap_or_else(|error| panic!("{width}x{height}: {error:#}"))
            .unwrap_or_else(|refusal| panic!("{width}x{height}: refused as {refusal:?}"));

        let taken = pixels(&cropped);
        assert_eq!((taken.0, taken.1), (rect.width, rect.height), "{width}x{height}: the crop is the wrong size");
        assert_same_pixels(&original, &taken, (rect.x, rect.y));
    }
}

/// A crop against each edge in turn.
///
/// The far edges are the ones that can be a partial MCU, and the near ones are
/// the ones whose DC coefficient has to be rebuilt from nothing. Cropping to
/// each corner exercises both halves without either hiding the other.
#[test]
fn a_crop_against_every_edge_holds() {
    let (width, height) = (300u32, 220u32);
    let bytes = a_jpeg(width, height, 88);
    let grid = grid_of(&bytes).expect("a baseline jpeg");
    let original = pixels(&bytes);

    let across = width / grid.width * grid.width;
    let down = height / grid.height * grid.height;

    for wanted in [
        // The four corners.
        Rect::new(0, 0, across / 2, down / 2),
        Rect::new(across / 2, 0, width - across / 2, down / 2),
        Rect::new(0, down / 2, across / 2, height - down / 2),
        Rect::new(across / 2, down / 2, width - across / 2, height - down / 2),
        // A band down the middle, and one across it.
        Rect::new(grid.width, 0, across - grid.width * 2, height),
        Rect::new(0, grid.height, width, down - grid.height * 2),
    ] {
        let rect = snap(wanted, (width, height), grid).unwrap_or_else(|| panic!("{wanted:?} could not be snapped"));
        let cropped = crop_losslessly(&bytes, rect)
            .unwrap_or_else(|error| panic!("{rect:?}: {error:#}"))
            .unwrap_or_else(|refusal| panic!("{rect:?}: refused as {refusal:?}"));

        let taken = pixels(&cropped);
        assert_eq!((taken.0, taken.1), (rect.width, rect.height), "{rect:?}: the crop is the wrong size");
        assert_same_pixels(&original, &taken, (rect.x, rect.y));
    }
}

/// A crop of a crop of a crop is still the original's pixels.
///
/// This is the promise a person actually cashes in — the reason to have a
/// lossless path at all is that cropping twice does not cost twice. A
/// re-encoding implementation passes the single-crop test on a good day and
/// fails this one every time.
#[test]
fn cropping_repeatedly_costs_nothing() {
    let (width, height) = (384u32, 288u32);
    let mut bytes = a_jpeg(width, height, 82);
    let grid = grid_of(&bytes).expect("a baseline jpeg");
    let original = pixels(&bytes);

    let mut offset = (0u32, 0u32);
    for _ in 0..5 {
        let (current_width, current_height, _) = pixels(&bytes);
        let wanted = Rect::new(
            grid.width,
            grid.height,
            current_width.saturating_sub(grid.width * 2),
            current_height.saturating_sub(grid.height * 2),
        );
        let Some(rect) = snap(wanted, (current_width, current_height), grid) else {
            break;
        };

        bytes = crop_losslessly(&bytes, rect)
            .expect("the crop ran")
            .expect("each of these is a baseline jpeg on the grid");
        offset = (offset.0 + rect.x, offset.1 + rect.y);
    }

    let taken = pixels(&bytes);
    assert!(taken.0 < width && taken.1 < height, "nothing was actually cropped");
    assert_same_pixels(&original, &taken, offset);
}

/// A file that cannot take the coefficient path says so, and the fallback
/// gives the right pixels anyway.
///
/// The two halves of the promise: the refusal is named rather than silent, and
/// choosing the other path does not mean choosing a different picture.
#[test]
fn a_file_off_the_coefficient_path_still_crops_correctly() {
    let mut pixels_in = image::RgbaImage::new(200, 150);
    let mut seed = 0x5DEE_CE66u32;
    for (x, y, pixel) in pixels_in.enumerate_pixels_mut() {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        *pixel = image::Rgba([(seed >> 24) as u8, (x * 255 / 200) as u8, (y * 255 / 150) as u8, 255]);
    }
    let mut png = std::io::Cursor::new(Vec::new());
    pixels_in.write_to(&mut png, image::ImageFormat::Png).expect("encoding the fixture");
    let png = png.into_inner();

    // A PNG carries no compression grid at all, which is what tells the viewer
    // to take the other path.
    assert!(grid_of(&png).is_none(), "a PNG was offered the coefficient path");

    // The fallback: decode, cut, and the cut must be the right rectangle.
    let loaded = decode_here(&png).expect("the viewer opens a PNG");
    let rect = (37u32, 21u32, 96u32, 64u32);
    let taken = cut(&loaded.image, nitid::testing::Orientation::Normal, rect).expect("the cut ran");

    assert_eq!((taken.width, taken.height), (rect.2, rect.3), "the cut is the wrong size");
    for row in 0..rect.3 {
        for column in 0..rect.2 {
            let from = (((row + rect.1) * loaded.image.width + column + rect.0) * 4) as usize;
            let to = ((row * rect.2 + column) * 4) as usize;
            assert_eq!(
                taken.pixels[to..to + 4],
                loaded.image.pixels[from..from + 4],
                "the pixel at {column},{row} is not the one that was there"
            );
        }
    }
}

/// The offer the interface makes must be one the crop can keep.
///
/// The bar tells a person what will happen before it happens; if `snap` could
/// name a rectangle that `crop` then refuses, the viewer would promise a
/// lossless crop and quietly re-encode — which is the exact silence this
/// feature is built to avoid.
#[test]
fn every_rectangle_the_snap_offers_is_one_the_crop_accepts() {
    let (width, height) = (300u32, 220u32);
    let bytes = a_jpeg(width, height, 90);
    let grid = grid_of(&bytes).expect("a baseline jpeg");

    // A spread of awkward rectangles, none of them on the grid to begin with.
    let mut offered = 0;
    for x in [0u32, 1, 7, 33, 100] {
        for y in [0u32, 3, 15, 61] {
            for w in [17u32, 64, 129, 300] {
                for h in [9u32, 48, 111, 220] {
                    let wanted = Rect::new(x, y, w.min(width.saturating_sub(x)), h.min(height.saturating_sub(y)));
                    let Some(rect) = snap(wanted, (width, height), grid) else {
                        continue;
                    };
                    offered += 1;
                    let outcome = crop_losslessly(&bytes, rect).unwrap_or_else(|error| panic!("{rect:?}: {error:#}"));
                    assert!(
                        outcome.is_ok(),
                        "the bar would offer {rect:?} as lossless and the crop refuses it as {:?}",
                        outcome.unwrap_err()
                    );
                }
            }
        }
    }
    assert!(offered > 50, "only {offered} rectangles were offered, so this stopped exercising the snap");
}

/// The subsampled case, which is what a camera writes and what the 16x16 grid
/// exists for.
///
/// Its own test rather than another row in the loop above, because its grid is
/// a different size and the rectangles have to be chosen against that. It is
/// also the one that dies on a mutation to the per-component block count: in a
/// 4:4:4 file every component is one block per MCU, so getting that count
/// wrong changes nothing at all.
#[test]
fn a_subsampled_jpeg_crops_losslessly_too() {
    let bytes = a_subsampled_jpeg();
    let grid = grid_of(&bytes).expect("the fixture is a baseline jpeg");
    assert_eq!(
        (grid.width, grid.height),
        (16, 16),
        "the fixture is meant to be 4:2:0, and its grid says otherwise — the whole point of this test is the subsampled path"
    );

    let original = pixels(&bytes);
    let (width, height) = (original.0, original.1);

    for wanted in [
        Rect::new(grid.width, grid.height, width - grid.width * 2, height - grid.height * 2),
        Rect::new(0, 0, width / 2, height / 2),
        Rect::new(width / 2, height / 2, width, height),
        Rect::new(grid.width * 2, 0, grid.width * 4, height),
    ] {
        let rect = snap(wanted, (width, height), grid).unwrap_or_else(|| panic!("{wanted:?} could not be snapped"));
        let cropped = crop_losslessly(&bytes, rect)
            .unwrap_or_else(|error| panic!("{rect:?}: {error:#}"))
            .unwrap_or_else(|refusal| panic!("{rect:?}: refused as {refusal:?}"));

        let taken = pixels(&cropped);
        assert_eq!((taken.0, taken.1), (rect.width, rect.height), "{rect:?}: the crop is the wrong size");
        assert_interior_is_identical(&original, &taken, (rect.x, rect.y));
    }

    // And the tables the coefficients are measured in are the file's own: the
    // half of "lossless" that comparing pixels cannot see, since a careful
    // re-encode can land on similar pixels from rebuilt tables.
    let rect = snap(Rect::new(grid.width, grid.height, width, height), (width, height), grid).expect("a rectangle");
    let cropped = crop_losslessly(&bytes, rect).expect("ran").expect("lossless");
    assert_eq!(
        quantisation_tables(&bytes),
        quantisation_tables(&cropped),
        "the quantisation tables changed, so an encoder ran over the picture"
    );
}

/// Every DQT segment's payload, which is what an encoder would replace.
fn quantisation_tables(bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut tables = Vec::new();
    let mut at = 2usize;
    while at + 3 < bytes.len() {
        if bytes[at] != 0xFF {
            at += 1;
            continue;
        }
        let marker = bytes[at + 1];
        if marker == 0xDA || marker == 0xD9 {
            break;
        }
        if marker == 0xD8 || marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            at += 2;
            continue;
        }
        let length = usize::from(u16::from_be_bytes([bytes[at + 2], bytes[at + 3]]));
        if marker == 0xDB {
            tables.push(bytes[at + 4..at + 2 + length].to_vec());
        }
        at += 2 + length;
    }
    tables
}

/// Cropping a subsampled file over and over stays free, as it does for 4:4:4.
#[test]
fn a_subsampled_jpeg_survives_being_cropped_repeatedly() {
    let mut bytes = a_subsampled_jpeg();
    let grid = grid_of(&bytes).expect("a baseline jpeg");
    let original = pixels(&bytes);

    let mut offset = (0u32, 0u32);
    for _ in 0..3 {
        let (current_width, current_height, _) = pixels(&bytes);
        let wanted = Rect::new(
            grid.width,
            grid.height,
            current_width.saturating_sub(grid.width * 2),
            current_height.saturating_sub(grid.height * 2),
        );
        let Some(rect) = snap(wanted, (current_width, current_height), grid) else {
            break;
        };
        bytes = crop_losslessly(&bytes, rect).expect("the crop ran").expect("a baseline jpeg on the grid");
        offset = (offset.0 + rect.x, offset.1 + rect.y);
    }

    let taken = pixels(&bytes);
    assert!(taken.0 < original.0 && taken.1 < original.1, "nothing was actually cropped");
    assert_interior_is_identical(&original, &taken, offset);
}
