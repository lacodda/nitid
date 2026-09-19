//! The release gate for baking a turn into the pixels, and for taking the
//! metadata out of a file.
//!
//! Both are operations that claim to change a file without changing its
//! picture, and both can be wrong in ways nothing else notices. So this gate
//! holds them to the only standard that means anything: **decode the result and
//! compare it, pixel by pixel, against the same turn done by an ordinary image
//! library.**
//!
//! That comparison is the whole point. The coefficient transform in
//! `src/jpeg_lossless.rs` rests on an identity about the discrete cosine
//! transform — transposing a block of pixels transposes its coefficients, and
//! mirroring one negates the coefficients whose index along the mirrored axis
//! is odd. Getting a sign wrong, or transposing without the sign, produces a
//! file that decodes perfectly well and shows something that is not the
//! picture. A test that only checked the file's dimensions, or that it decodes
//! at all, would pass on all of it.
//!
//! # The standard each case is held to
//!
//! Stated exactly, because it is not quite "the same pixels" and the difference
//! is the interesting part.
//!
//! - **The coefficients are the file's own, permuted and never altered.** The
//!   proof is [`turning_four_times_gives_back_the_original_bytes`]: four quarter
//!   turns reproduce the original file *byte for byte*, scan included, on both a
//!   4:4:4 and a 4:2:0 fixture. Nothing that changed a coefficient could survive
//!   that.
//! - **Decoded pixels match an ordinary rotation to within 3 of 255**, and for
//!   the four turns that do not transpose, to within 2. Measured, not allowed
//!   for: a mirror moves no coefficient at all and a vertical flip comes out
//!   exact, while a transposing turn leaves a residue because "rotate then
//!   decode" and "decode then rotate" round differently in the final conversion
//!   to integers. That is the IDCT, not the transform under test.
//! - **The geometry is exactly an ordinary rotation.** A clockwise quarter turn
//!   of a mark in the top-left corner puts it in the top-right, and each of the
//!   eight symmetries is distinguishable on the fixture used — which is the
//!   check that a turn is not quietly its own inverse.
//! - **The quantisation tables are transposed with the coefficients**, and
//!   otherwise copied. A table is a quantiser per frequency *position*; moving
//!   coefficient `(u, v)` to `(v, u)` and leaving the table alone divides every
//!   one of them by its neighbour's quantiser. The tables encoders write are not
//!   symmetric about the diagonal, so this is not a theoretical concern: it
//!   measured 15 of 255, everywhere, before it was fixed.

use nitid::testing::{Format, Keep, Turn, can_scrub, decode_here, scrub, survey, turn_losslessly};

/// A picture with structure in it, encoded by the `image` crate.
///
/// Noise plus a hard-edged checkerboard plus a gradient: a smooth picture
/// compresses to so few non-zero coefficients that a turn which mangled the
/// high-frequency terms would still decode to something that looks right.
/// Asymmetric on purpose — a picture symmetric about either axis cannot tell a
/// quarter turn from its opposite, which is exactly the mistake most likely to
/// be made here.
fn a_jpeg(width: u32, height: u32, quality: u8) -> Vec<u8> {
    let mut pixels = image::RgbImage::new(width, height);
    let mut seed = 0x9E37_79B9u32;
    for (x, y, pixel) in pixels.enumerate_pixels_mut() {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        let noise = (seed >> 24) as u8;
        let edge = if (x / 16 + y / 16) % 2 == 0 { 40u8 } else { 200 };
        // The red channel rises with x and the blue with y at different rates,
        // so no two of the eight symmetries of this picture look alike.
        *pixel = image::Rgb([((x * 255) / width.max(1)) as u8, noise / 3 + edge / 2, ((y * 200) / height.max(1)) as u8]);
    }

    let mut out = Vec::new();
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(std::io::Cursor::new(&mut out), quality);
    image::ImageEncoder::write_image(encoder, pixels.as_raw(), width, height, image::ExtendedColorType::Rgb8).expect("the fixture encoded");
    out
}

/// The committed 4:2:0 file: a real subsampled JPEG, written by libjpeg through
/// Pillow rather than by anything in this crate.
///
/// The reason it is committed rather than built is recorded in `live_crop.rs`:
/// the `image` crate writes 4:4:4 only, so every fixture built in code leaves
/// the subsampled path — the one cameras actually write — untested.
fn a_subsampled_jpeg() -> Vec<u8> {
    std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/subsampled-420.jpg")).expect("the committed 4:2:0 fixture")
}

/// A 4:2:2 file, whose sampling factors are **asymmetric**: 2x1.
///
/// The fixture that exists for one reason. A turn that swaps the axes must swap
/// every component's sampling factors with them — 2x1 becomes 1x2 — and a frame
/// header left stating the old pair describes a block order the scan no longer
/// has. Neither of the other two fixtures can catch that: 4:4:4 is 1x1 and 4:2:0
/// is 2x2, and both are their own transpose, so the swap is invisible in them.
///
/// Measured, not assumed: removing the swap left all ten tests here green until
/// this file was added.
fn a_422_jpeg() -> Vec<u8> {
    std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/subsampled-422.jpg")).expect("the committed 4:2:2 fixture")
}

/// Decode through the viewer's own front door.
fn pixels(bytes: &[u8]) -> (u32, u32, Vec<u8>) {
    let loaded = decode_here(bytes).expect("the viewer decoded it");
    (loaded.image.width, loaded.image.height, loaded.image.pixels.clone())
}

/// The same turn, done the ordinary way: decode, permute pixels, and hand back
/// the result to compare against.
///
/// Written here rather than taken from `image`'s own `rotate90` so that the
/// four mirrored symmetries are covered by the same function, and so that the
/// reference is plainly readable as "this is what the turn means".
fn turned_by_hand(size: (u32, u32), rgba: &[u8], turn: Turn) -> ((u32, u32), Vec<u8>) {
    let (width, height) = size;
    let (out_width, out_height) = if turn.swaps_axes() { (height, width) } else { (width, height) };
    let mut out = vec![0u8; (out_width as usize) * (out_height as usize) * 4];

    for y in 0..height {
        for x in 0..width {
            let (to_x, to_y) = match turn {
                Turn::None => (x, y),
                Turn::Quarter => (height - 1 - y, x),
                Turn::Half => (width - 1 - x, height - 1 - y),
                Turn::ThreeQuarters => (y, width - 1 - x),
                Turn::FlipHorizontal => (width - 1 - x, y),
                Turn::FlipVertical => (x, height - 1 - y),
                Turn::Transpose => (y, x),
                Turn::Transverse => (height - 1 - y, width - 1 - x),
            };
            let from = ((y as usize) * (width as usize) + x as usize) * 4;
            let to = ((to_y as usize) * (out_width as usize) + to_x as usize) * 4;
            out[to..to + 4].copy_from_slice(&rgba[from..from + 4]);
        }
    }

    ((out_width, out_height), out)
}

/// Hold a baked turn to an ordinary rotation of the same picture.
///
/// `tolerance` is the largest per-channel difference allowed, and it is a
/// measured number rather than a comfortable one. A turn that transposes leaves
/// a residue of up to 3 of 255 because the comparison decodes twice along
/// different routes — "rotate the coefficients, then decode" against "decode,
/// then rotate the pixels" — and the two round differently where the inverse
/// transform lands on integers. A turn that only mirrors moves no coefficient
/// at all and comes in under 2.
///
/// The residue is bounded *and* the geometry is exact, which together say the
/// transform is right. That the coefficients themselves are untouched is a
/// separate and stronger claim, proved by
/// [`turning_four_times_gives_back_the_original_bytes`] on the bytes.
fn assert_matches_a_plain_rotation(original: &[u8], baked: &[u8], turn: Turn, tolerance: i32) {
    let (width, height, before) = pixels(original);
    let ((expected_width, expected_height), expected) = turned_by_hand((width, height), &before, turn);
    let (got_width, got_height, got) = pixels(baked);

    assert_eq!(
        (got_width, got_height),
        (expected_width, expected_height),
        "{turn:?}: the turned file is {got_width}x{got_height}, not {expected_width}x{expected_height}"
    );

    let mut worst = 0i32;
    let mut worst_at = 0usize;
    for (at, (want, have)) in expected.chunks(4).zip(got.chunks(4)).enumerate() {
        let difference = (0..3)
            .map(|channel| (i32::from(want[channel]) - i32::from(have[channel])).abs())
            .max()
            .unwrap_or(0);
        if difference > worst {
            worst = difference;
            worst_at = at;
        }
    }

    assert!(
        worst <= tolerance,
        "{turn:?}: a pixel is {worst} off, over the {tolerance} this turn is allowed; the worst is at ({}, {})",
        worst_at as u32 % expected_width,
        worst_at as u32 / expected_width,
    );
}

/// Where a mark in one corner ends up, as four booleans.
///
/// The check that catches a turn that goes the wrong way round. A turn of the
/// wrong handedness has the right dimensions, decodes cleanly, and comes back to
/// the original after four applications — the group does not care which way
/// round it goes. Only the geometry says, and only on a picture whose corners
/// are distinguishable.
fn corners(bytes: &[u8]) -> (bool, bool, bool, bool) {
    let (width, height, pixels) = pixels(bytes);
    let bright = |x: u32, y: u32| pixels[((y * width + x) * 4) as usize] > 128;
    (bright(0, 0), bright(width - 1, 0), bright(0, height - 1), bright(width - 1, height - 1))
}

/// A picture that is dark but for a bright square in its top-left corner.
fn marked_top_left(width: u32, height: u32) -> Vec<u8> {
    let mut pixels = image::RgbImage::new(width, height);
    for (x, y, pixel) in pixels.enumerate_pixels_mut() {
        *pixel = if x < 8 && y < 8 { image::Rgb([255, 255, 255]) } else { image::Rgb([0, 0, 0]) };
    }
    let mut out = Vec::new();
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(std::io::Cursor::new(&mut out), 100);
    image::ImageEncoder::write_image(encoder, pixels.as_raw(), width, height, image::ExtendedColorType::Rgb8).expect("the fixture encoded");
    out
}

/// The zig-zag order JPEG stores a block and a quantisation table in.
const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29,
    22, 15, 23, 30, 37, 44, 51, 58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

/// Every quantisation table in the file, de-zigzagged into row-major order.
///
/// De-zigzagged rather than left as stored, because a transpose is about
/// position and the zig-zag order says nothing about position: comparing the
/// stored bytes would be comparing two scrambles and hoping.
///
/// Eight-bit tables only, which is what every encoder here writes; a sixteen-bit
/// table would be skipped, and the assertion that the fixture has an asymmetric
/// table is what would notice.
fn quantisation_tables(bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut tables = Vec::new();
    let mut at = 2usize;
    while at + 3 < bytes.len() {
        if bytes[at] != 0xFF {
            at += 1;
            continue;
        }
        let marker = bytes[at + 1];
        if marker == 0xDA {
            break;
        }
        if marker == 0xD8 || marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            at += 2;
            continue;
        }
        let length = usize::from(u16::from_be_bytes([bytes[at + 2], bytes[at + 3]]));
        if length < 2 || at + 2 + length > bytes.len() {
            break;
        }
        if marker == 0xDB {
            let body = &bytes[at + 4..at + 2 + length];
            let mut offset = 0usize;
            while offset + 65 <= body.len() {
                // Eight-bit precision only; anything else is not walked.
                if body[offset] >> 4 != 0 {
                    break;
                }
                let stored = &body[offset + 1..offset + 65];
                let mut natural = vec![0u8; 64];
                for (index, value) in stored.iter().enumerate() {
                    natural[ZIGZAG[index]] = *value;
                }
                tables.push(natural);
                offset += 65;
            }
        }
        at += 2 + length;
    }
    tables
}

/// Each component's sampling factors, as the frame header states them.
fn sampling_factors(bytes: &[u8]) -> Vec<(u8, u8)> {
    let mut at = 2usize;
    while at + 3 < bytes.len() {
        if bytes[at] != 0xFF {
            at += 1;
            continue;
        }
        let marker = bytes[at + 1];
        if marker == 0xDA {
            break;
        }
        if marker == 0xD8 || marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            at += 2;
            continue;
        }
        let length = usize::from(u16::from_be_bytes([bytes[at + 2], bytes[at + 3]]));
        if length < 2 || at + 2 + length > bytes.len() {
            break;
        }
        if marker == 0xC0 || marker == 0xC1 {
            let count = usize::from(bytes[at + 9]);
            return (0..count)
                .map(|index| {
                    let packed = bytes[at + 10 + index * 3 + 1];
                    (packed >> 4, packed & 0x0F)
                })
                .collect();
        }
        at += 2 + length;
    }
    Vec::new()
}

/// A row-major 8x8 table, transposed.
fn transposed(table: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; 64];
    for row in 0..8 {
        for column in 0..8 {
            out[column * 8 + row] = table[row * 8 + column];
        }
    }
    out
}

/// Whether a table is its own transpose, in which case it cannot tell a correct
/// implementation from one that forgot to transpose it.
fn is_symmetric(table: &[u8]) -> bool {
    (0..8).all(|row| (0..8).all(|column| table[row * 8 + column] == table[column * 8 + row]))
}

/// Every one of the eight turns, against the reference, on a 4:4:4 file.
///
/// The mirrored four are here as well as the rotations because they share the
/// sign logic: a quarter turn is a transpose plus one mirror, so a sign error
/// shows up in one of these eight and the set is what makes it findable.
#[test]
fn every_turn_matches_an_ordinary_rotation_exactly() {
    // 96x64 at quality 92: both dimensions a multiple of 8, so a 4:4:4 file
    // tiles its grid exactly and every turn is available.
    let original = a_jpeg(96, 64, 92);

    for turn in [
        Turn::Quarter,
        Turn::Half,
        Turn::ThreeQuarters,
        Turn::FlipHorizontal,
        Turn::FlipVertical,
        Turn::Transpose,
        Turn::Transverse,
    ] {
        let baked = turn_losslessly(&original, turn)
            .expect("the turn ran")
            .unwrap_or_else(|refusal| panic!("{turn:?} was refused: {}", refusal.reason()));

        // A mirror moves no coefficient, so it is held to the tighter figure.
        assert_matches_a_plain_rotation(&original, &baked, turn, if turn.swaps_axes() { 3 } else { 2 });
    }
}

/// The subsampled case, which is what a camera writes.
///
/// A 4:2:0 file has a chroma plane half the size of its luma plane and 16x16
/// MCUs, so the block bookkeeping is different in every particular from the
/// 4:4:4 case above — and it is the one that got no coverage at all before the
/// fixture was committed. Unlike a crop, a turn of this file is *exact*: every
/// block survives, so no chroma edge is interpolated against a neighbour that
/// has gone.
#[test]
fn a_subsampled_jpeg_turns_exactly_too() {
    let original = a_subsampled_jpeg();

    for turn in [Turn::Quarter, Turn::Half, Turn::ThreeQuarters, Turn::Transpose] {
        let baked = turn_losslessly(&original, turn)
            .expect("the turn ran")
            .unwrap_or_else(|refusal| panic!("{turn:?} was refused on 4:2:0: {}", refusal.reason()));

        assert_matches_a_plain_rotation(&original, &baked, turn, if turn.swaps_axes() { 3 } else { 2 });
    }
}

/// The case only an asymmetric subsampling can state: the sampling factors
/// travel with the axes.
///
/// A 4:2:2 file is sampled 2x1 — two luma blocks across per MCU, one down. Turn
/// it a quarter, and it must become 1x2. Leave the header saying 2x1 and the
/// decoder reads the blocks in the wrong order: the picture comes out as bands
/// of misplaced colour.
///
/// This test is here because its absence was measured. Deleting the swap from
/// `Frame::turned` left every other test in this file green, because 4:4:4
/// (1x1) and 4:2:0 (2x2) are both symmetric under it.
#[test]
fn an_asymmetric_subsampling_keeps_its_sampling_factors_with_its_axes() {
    let original = a_422_jpeg();
    assert_eq!(
        sampling_factors(&original),
        vec![(2, 1), (1, 1), (1, 1)],
        "the fixture is not the 4:2:2 this test is about"
    );

    for turn in [Turn::Quarter, Turn::ThreeQuarters, Turn::Transpose, Turn::Transverse] {
        let baked = turn_losslessly(&original, turn)
            .expect("the turn ran")
            .unwrap_or_else(|refusal| panic!("{turn:?} was refused on 4:2:2: {}", refusal.reason()));

        assert_eq!(
            sampling_factors(&baked),
            vec![(1, 2), (1, 1), (1, 1)],
            "{turn:?}: the header still states the sampling of the picture before the turn"
        );
        assert_matches_a_plain_rotation(&original, &baked, turn, 3);
    }

    // A turn that does not swap the axes must leave them alone.
    for turn in [Turn::Half, Turn::FlipHorizontal, Turn::FlipVertical] {
        let baked = turn_losslessly(&original, turn).expect("ran").expect("refused");
        assert_eq!(
            sampling_factors(&baked),
            vec![(2, 1), (1, 1), (1, 1)],
            "{turn:?} swapped the sampling factors without swapping the axes"
        );
        assert_matches_a_plain_rotation(&original, &baked, turn, 2);
    }

    // And it is reversible on the bytes, like the others.
    let mut carried = original.clone();
    for _ in 0..4 {
        carried = turn_losslessly(&carried, Turn::Quarter).expect("ran").expect("refused");
    }
    assert_eq!(carried, original, "four quarter turns of a 4:2:2 file did not give back its bytes");
}

/// The proof that nothing is lost: four quarter turns give back the original
/// **file**, byte for byte.
///
/// This is the assertion the whole module rests on, and it is much stronger than
/// the pixel comparisons above. Those decode twice and therefore carry the
/// decoder's rounding with them. This one never decodes at all: if any step
/// altered a coefficient, scaled one, dropped a high-frequency term or rounded
/// anything, four applications could not land on the original's own bytes.
///
/// Held on both subsamplings, and on the whole file rather than only the scan —
/// so the quantisation tables, the Huffman tables and the headers are included
/// in the claim.
#[test]
fn turning_four_times_gives_back_the_original_bytes() {
    for (name, original) in [("4:4:4", a_jpeg(96, 64, 90)), ("4:2:0", a_subsampled_jpeg())] {
        let mut carried = original.clone();
        for step in 1..=4 {
            carried = turn_losslessly(&carried, Turn::Quarter)
                .expect("the turn ran")
                .unwrap_or_else(|refusal| panic!("{name}: turn {step} was refused: {}", refusal.reason()));
        }
        assert_eq!(carried, original, "{name}: four quarter turns did not give back the original file");

        // The same for the turns that are their own inverse, which exercise the
        // mirror path and the transpose path separately.
        for turn in [Turn::Half, Turn::Transpose, Turn::FlipHorizontal, Turn::FlipVertical, Turn::Transverse] {
            let once = turn_losslessly(&original, turn)
                .expect("the turn ran")
                .unwrap_or_else(|refusal| panic!("{name} {turn:?} was refused: {}", refusal.reason()));
            let twice = turn_losslessly(&once, turn).expect("the turn ran").expect("the second turn was refused");
            assert_eq!(twice, original, "{name}: {turn:?} twice did not give back the original file");
        }

        // And a quarter turn each way cancels.
        let clockwise = turn_losslessly(&original, Turn::Quarter).expect("ran").expect("refused");
        let back = turn_losslessly(&clockwise, Turn::ThreeQuarters).expect("ran").expect("refused");
        assert_eq!(back, original, "{name}: a quarter turn each way did not cancel");
    }
}

/// Which way round each turn goes, asked of the picture rather than of the code.
///
/// The check that a turn is not its own mirror image. A quarter turn implemented
/// anticlockwise has the right dimensions, decodes cleanly, and satisfies every
/// reversibility test above — four anticlockwise turns come back just as well as
/// four clockwise ones. It was written the wrong way round here first, and this
/// is what said so.
#[test]
fn each_turn_goes_the_way_its_name_says() {
    let original = marked_top_left(32, 24);
    assert_eq!(
        corners(&original),
        (true, false, false, false),
        "the fixture's mark is not where the test thinks"
    );

    // (top-left, top-right, bottom-left, bottom-right)
    for (turn, expected) in [
        (Turn::Quarter, (false, true, false, false)),
        (Turn::ThreeQuarters, (false, false, true, false)),
        (Turn::Half, (false, false, false, true)),
        (Turn::Transpose, (true, false, false, false)),
        (Turn::Transverse, (false, false, false, true)),
        (Turn::FlipHorizontal, (false, true, false, false)),
        (Turn::FlipVertical, (false, false, true, false)),
    ] {
        let baked = turn_losslessly(&original, turn)
            .expect("the turn ran")
            .unwrap_or_else(|refusal| panic!("{turn:?} was refused: {}", refusal.reason()));
        assert_eq!(corners(&baked), expected, "{turn:?} moved the corner mark to the wrong place");
    }
}

/// The quantisation tables are transposed with the coefficients, and only then.
///
/// A table is a quantiser per frequency position. Transposing coefficients
/// without transposing the table divides each one by its neighbour's quantiser,
/// which decodes to a picture that is right everywhere and correct nowhere —
/// measured at 15 of 255 before this was fixed. A mirror moves nothing, so its
/// tables must come across untouched, and asserting *that* is what stops the
/// transpose being applied where it does not belong.
#[test]
fn the_quantisation_tables_are_transposed_only_when_the_coefficients_are() {
    let original = a_jpeg(96, 64, 92);
    let before = quantisation_tables(&original);
    assert!(!before.is_empty(), "the fixture carries no quantisation tables");

    // At least one table must be asymmetric, or this test proves nothing: a
    // symmetric table is its own transpose and every version of the code passes.
    assert!(
        before.iter().any(|table| !is_symmetric(table)),
        "every table in the fixture is symmetric under transpose, so this test cannot fail"
    );

    for turn in [Turn::Quarter, Turn::ThreeQuarters, Turn::Transpose, Turn::Transverse] {
        let baked = turn_losslessly(&original, turn).expect("ran").expect("refused");
        let after = quantisation_tables(&baked);
        assert_eq!(after.len(), before.len(), "{turn:?} changed how many tables there are");
        for (index, (was, now)) in before.iter().zip(after.iter()).enumerate() {
            assert_eq!(now, &transposed(was), "{turn:?}: table {index} was not transposed with the coefficients");
        }
    }

    for turn in [Turn::Half, Turn::FlipHorizontal, Turn::FlipVertical] {
        let baked = turn_losslessly(&original, turn).expect("ran").expect("refused");
        assert_eq!(
            quantisation_tables(&baked),
            before,
            "{turn:?} transposed the tables, and it moves no coefficient"
        );
    }
}

/// A turn that would bring the padded edge of a partial MCU inside the picture
/// must refuse rather than produce a seam.
///
/// A JPEG's last block in a row is partial: the pixels past the picture's edge
/// are padding a decoder throws away. That is harmless while the edge stays an
/// edge. Turn it, and the padding lands in the middle of the picture. The
/// honest answer is the encoder, and the refusal is what sends the caller
/// there.
#[test]
fn a_turn_that_would_show_the_padding_refuses_by_name() {
    // 100x60 at 4:4:4: the grid is 8x8, and neither dimension is a multiple of
    // it, so a turn would move a partial edge inwards.
    let ragged = a_jpeg(100, 60, 90);
    let refusal = turn_losslessly(&ragged, Turn::Quarter)
        .expect("the turn ran")
        .expect_err("a ragged file was turned anyway");
    assert_eq!(refusal, nitid::testing::Refusal::OffGrid, "the refusal named the wrong cause");
    // The message a person would read says what it is about.
    assert!(refusal.reason().contains("grid"), "the reason does not mention the grid: {}", refusal.reason());

    // And a file whose dimensions do tile the grid is turned.
    assert!(turn_losslessly(&a_jpeg(96, 64, 90), Turn::Quarter).expect("the turn ran").is_ok());
}

/// Baking a turn clears the orientation tag, or every viewer turns it twice.
///
/// The one field a bake must rewrite. Read back through the viewer's own EXIF
/// reader, so the assertion is about what a program will actually see rather
/// than about the bytes this test expects.
#[test]
fn a_baked_turn_leaves_the_file_saying_it_is_upright() {
    // A JPEG carrying an orientation, built by writing one into a file the way
    // the viewer's own `Ctrl+S` does.
    let directory = std::env::temp_dir().join(format!("nitid-bake-{}", std::process::id()));
    std::fs::create_dir_all(&directory).expect("a directory");
    let path = directory.join("sideways.jpg");
    std::fs::write(&path, a_jpeg(96, 64, 90)).expect("the fixture written");
    nitid::testing::save_orientation(&path, nitid::testing::Orientation::Rotate90).expect("the orientation written");

    let sideways = std::fs::read(&path).expect("the fixture read back");
    assert_eq!(
        decode_here(&sideways).expect("it decoded").orientation,
        nitid::testing::Orientation::Rotate90,
        "the fixture does not carry the orientation the test is about"
    );

    let baked = turn_losslessly(&sideways, Turn::of(nitid::testing::Orientation::Rotate90))
        .expect("the turn ran")
        .expect("the turn was refused");

    assert_eq!(
        decode_here(&baked).expect("the baked file decoded").orientation,
        nitid::testing::Orientation::Normal,
        "the baked file still asks to be turned, so it will be shown turned twice"
    );

    let _ = std::fs::remove_dir_all(&directory);
}

/// What a bake is *for*: the picture a viewer shows must not change.
///
/// The composition of "turn the pixels the way the tag asked" and "clear the
/// tag" is the identity as far as anyone looking at the file is concerned. This
/// is the promise a person cares about, and it is the one that would break
/// silently if `Turn::of` inverted the wrong way — `Rotate90` and `Rotate270`
/// are each other's inverse, and both look right on a picture that is square.
#[test]
fn baking_shows_the_same_picture_the_tag_asked_for() {
    let directory = std::env::temp_dir().join(format!("nitid-bake-same-{}", std::process::id()));
    std::fs::create_dir_all(&directory).expect("a directory");

    for orientation in [
        nitid::testing::Orientation::Rotate90,
        nitid::testing::Orientation::Rotate180,
        nitid::testing::Orientation::Rotate270,
        nitid::testing::Orientation::FlipHorizontal,
        nitid::testing::Orientation::Transpose,
    ] {
        let path = directory.join(format!("{orientation:?}.jpg"));
        std::fs::write(&path, a_jpeg(96, 64, 92)).expect("the fixture written");
        nitid::testing::save_orientation(&path, orientation).expect("the orientation written");
        let tagged = std::fs::read(&path).expect("read back");

        // What the viewer shows for the tagged file: its stored pixels with the
        // orientation applied.
        let loaded = decode_here(&tagged).expect("it decoded");
        let shown = turned_by_hand((loaded.image.width, loaded.image.height), &loaded.image.pixels, Turn::of(orientation));

        let baked = turn_losslessly(&tagged, Turn::of(orientation))
            .expect("the turn ran")
            .unwrap_or_else(|refusal| panic!("{orientation:?} was refused: {}", refusal.reason()));
        let after = decode_here(&baked).expect("the baked file decoded");

        assert_eq!(
            after.orientation,
            nitid::testing::Orientation::Normal,
            "{orientation:?}: the tag was not cleared"
        );
        assert_eq!(
            (after.image.width, after.image.height),
            shown.0,
            "{orientation:?}: the baked file is not the shape the tagged one was shown at"
        );

        // To the same measured tolerance as the turns above, and for the same
        // reason: both sides of this comparison have been through the inverse
        // transform, by different routes.
        let allowed = if Turn::of(orientation).swaps_axes() { 3 } else { 2 };
        let worst = shown
            .1
            .chunks(4)
            .zip(after.image.pixels.chunks(4))
            .map(|(want, have)| {
                (0..3)
                    .map(|channel| (i32::from(want[channel]) - i32::from(have[channel])).abs())
                    .max()
                    .unwrap_or(0)
            })
            .max()
            .unwrap_or(0);
        assert!(
            worst <= allowed,
            "{orientation:?}: the baked file shows something {worst} away from what the tagged one showed, over the {allowed} allowed"
        );
    }

    let _ = std::fs::remove_dir_all(&directory);
}

// ---------------------------------------------------------------------------
// Scrubbing, over files a third party wrote
// ---------------------------------------------------------------------------

/// A scrubbed file must still open, and still be the same picture.
///
/// The unit tests in `src/scrub.rs` build their fixtures by hand, which is what
/// makes them precise about the segments. This runs the scrub over files an
/// ordinary encoder wrote and opens the result through the viewer's own front
/// door — the check that the file is still a file.
#[test]
fn a_scrubbed_file_is_the_same_picture_and_still_opens() {
    let directory = std::env::temp_dir().join(format!("nitid-scrub-{}", std::process::id()));
    std::fs::create_dir_all(&directory).expect("a directory");

    // A JPEG with EXIF in it, written by the viewer's own EXIF writer over a
    // third party's encoding.
    let path = directory.join("photo.jpg");
    std::fs::write(&path, a_jpeg(96, 64, 90)).expect("written");
    nitid::testing::save_orientation(&path, nitid::testing::Orientation::Rotate180).expect("EXIF written");
    let with_exif = std::fs::read(&path).expect("read back");

    let found = survey(&with_exif, Format::Jpeg).expect("a survey");
    assert!(found.exif, "the fixture carries no EXIF, so the test would pass for nothing");

    let scrubbed = scrub(&with_exif, Format::Jpeg, Keep::Profile).expect("a scrub");
    let after = survey(&scrubbed, Format::Jpeg).expect("a survey");
    assert!(!after.exif, "the EXIF survived the scrub");

    // The picture itself is untouched: same size, same pixels, and it opens.
    let (width, height, before) = pixels(&with_exif);
    let (scrubbed_width, scrubbed_height, pixels_after) = pixels(&scrubbed);
    assert_eq!((scrubbed_width, scrubbed_height), (width, height));
    assert_eq!(pixels_after, before, "the scrub changed the picture");

    // A PNG, whose metadata `image` does not write — so this is the case where
    // a survey must honestly report nothing rather than inventing something.
    let mut png = Vec::new();
    let pixels_in = image::RgbaImage::from_pixel(8, 8, image::Rgba([10, 20, 30, 255]));
    image::ImageEncoder::write_image(
        image::codecs::png::PngEncoder::new(std::io::Cursor::new(&mut png)),
        pixels_in.as_raw(),
        8,
        8,
        image::ExtendedColorType::Rgba8,
    )
    .expect("a PNG");
    let found = survey(&png, Format::Png).expect("a survey");
    assert!(!found.anything(Keep::Profile), "a plain PNG was reported as carrying metadata: {found:?}");
    let scrubbed = scrub(&png, Format::Png, Keep::Profile).expect("a scrub");
    assert_eq!(pixels(&scrubbed).2, pixels(&png).2, "scrubbing a plain PNG changed it");

    let _ = std::fs::remove_dir_all(&directory);
}

/// The formats that cannot be scrubbed must say so rather than appear to work.
#[test]
fn a_format_that_cannot_be_scrubbed_is_known_in_advance() {
    for format in [Format::Heic, Format::Avif, Format::JpegXl, Format::Gif, Format::Tiff, Format::Bmp, Format::Svg] {
        assert!(!can_scrub(format), "{format:?} was offered a scrub it cannot do");
    }
    for format in [Format::Jpeg, Format::Png, Format::WebP] {
        assert!(can_scrub(format), "{format:?} can be scrubbed and was not offered");
    }
}
