//! Reading one pixel of the picture, as the file stores it.
//!
//! What a colour *is* has two answers here, and the eyedropper gives both: the
//! numbers the file holds, and what they become on this display. They differ
//! whenever the image carries a profile the display does not share, and a
//! viewer that reported only one of them would be answering a question the
//! person did not ask — "what colour is this" means the file's value to
//! someone matching a brand colour, and the display's to someone matching what
//! they see.
//!
//! The file's value is the one that goes to the clipboard, for the same reason
//! the histogram counts the file (ADR 0019): it is a fact about the picture,
//! and it does not change when the window moves to another monitor.

use crate::color::ColorTransform;
use crate::image_source::{DecodedImage, Depth, Orientation};

/// One pixel, in both the terms that matter.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Reading {
    /// Where in the image this was read, in the picture's own pixels after
    /// orientation — the coordinates the person sees, not the ones the file
    /// happens to store the pixel at.
    pub at: (u32, u32),
    /// The channels as the file stores them, scaled to 0..255 for display.
    ///
    /// A sixteen-bit file is reported at eight bits here and at its own depth
    /// in [`raw`](Self::raw): the hex a person copies is eight-bit, and a
    /// number in 0..65535 is not what anyone means by "the colour".
    pub file: [u8; 3],
    /// The same pixel after the colour transform, as the display shows it.
    ///
    /// Equal to `file` when the image needs no conversion, which is the common
    /// case and the one where the panel says so rather than repeating itself.
    pub display: [u8; 3],
    /// Alpha as stored, 0..255.
    pub alpha: u8,
    /// The file's own values at their full depth, for a caller that wants the
    /// precision an eight-bit reading throws away.
    pub raw: [u16; 3],
    /// The depth those raw values are in.
    pub depth: Depth,
}

impl Reading {
    /// The file's colour as a hex string, which is what goes to the clipboard.
    pub fn hex(&self) -> String {
        format!("#{:02X}{:02X}{:02X}", self.file[0], self.file[1], self.file[2])
    }

    /// Whether the display shows this pixel as something other than what the
    /// file holds.
    ///
    /// A rounding step apart is not a difference worth reporting: the two
    /// paths reach eight bits by different arithmetic, and a panel that
    /// claimed a conversion for a value one off would cry wolf on every
    /// untagged image.
    pub fn converted(&self) -> bool {
        self.file.iter().zip(&self.display).any(|(file, display)| file.abs_diff(*display) > 1)
    }
}

/// Read the pixel at `at`, which is in the picture's oriented coordinates.
///
/// `orientation` is what the file asks for combined with any turn the user
/// made, so this is the inverse of what the shader does to texture
/// coordinates: the person points at the picture as shown, and the pixel lives
/// in the image as stored.
pub fn read(image: &DecodedImage, orientation: Orientation, transform: &ColorTransform, at: (u32, u32)) -> Option<Reading> {
    let shown = shown_size(image, orientation);
    if at.0 >= shown.0 || at.1 >= shown.1 {
        return None;
    }

    let stored = to_stored(at, shown, orientation);
    let index = (stored.1 as usize) * (image.width as usize) + (stored.0 as usize);

    let (raw, alpha) = match image.depth {
        Depth::Eight => {
            let offset = index * 4;
            let bytes = image.pixels.get(offset..offset + 4)?;
            ([u16::from(bytes[0]), u16::from(bytes[1]), u16::from(bytes[2])], bytes[3])
        }
        Depth::Sixteen => {
            let offset = index * 8;
            let bytes = image.pixels.get(offset..offset + 8)?;
            let sample = |channel: usize| u16::from_ne_bytes([bytes[channel * 2], bytes[channel * 2 + 1]]);
            // Alpha is reported at eight bits like the rest of the display
            // values; the full-depth channels are in `raw`.
            ((sample(0), sample(1), sample(2)).into(), (sample(3) >> 8) as u8)
        }
    };

    let file = [to_eight(raw[0], image.depth), to_eight(raw[1], image.depth), to_eight(raw[2], image.depth)];
    let display = through(transform, raw, image.depth);

    Some(Reading {
        at,
        file,
        display,
        alpha,
        raw,
        depth: image.depth,
    })
}

/// The picture's size as shown, which is the stored size with the axes
/// exchanged for a quarter turn.
fn shown_size(image: &DecodedImage, orientation: Orientation) -> (u32, u32) {
    if orientation.swaps_axes() {
        (image.height, image.width)
    } else {
        (image.width, image.height)
    }
}

/// Where a pixel of the picture-as-shown lives in the image as stored.
///
/// The inverse of the orientation. Written out per case rather than derived
/// from the matrix: the mirrored orientations are the ones that go wrong, they
/// are the ones real files carry, and a table that can be checked by
/// exhaustive round trip is worth more here than a derivation that looks
/// right. The same reasoning as ADR 0018, reached from the other end.
fn to_stored(at: (u32, u32), shown: (u32, u32), orientation: Orientation) -> (u32, u32) {
    let (x, y) = at;
    // The last valid coordinate on each axis of the shown picture.
    let (last_x, last_y) = (shown.0.saturating_sub(1), shown.1.saturating_sub(1));

    match orientation {
        Orientation::Normal => (x, y),
        Orientation::FlipHorizontal => (last_x - x, y),
        Orientation::Rotate180 => (last_x - x, last_y - y),
        Orientation::FlipVertical => (x, last_y - y),
        // The four that exchange the axes: the shown x is the stored y.
        Orientation::Transpose => (y, x),
        Orientation::Rotate90 => (y, last_x - x),
        Orientation::Transverse => (last_y - y, last_x - x),
        Orientation::Rotate270 => (last_y - y, x),
    }
}

/// A stored sample as an eight-bit value.
fn to_eight(sample: u16, depth: Depth) -> u8 {
    match depth {
        Depth::Eight => sample as u8,
        Depth::Sixteen => (sample >> 8) as u8,
    }
}

/// The same pixel after the colour transform, in eight-bit display terms.
///
/// The arithmetic the shader does, on one pixel: decode through the source
/// curves to linear light, move between primaries, re-encode for sRGB. Done
/// here on the CPU rather than read back from the GPU because reading one
/// pixel off a surface costs a stall of the whole pipeline, and this is asked
/// for on every mouse move while the eyedropper is up.
fn through(transform: &ColorTransform, raw: [u16; 3], depth: Depth) -> [u8; 3] {
    let normalised = |channel: usize| match depth {
        Depth::Eight => f32::from(raw[channel]) / 255.0,
        Depth::Sixteen => f32::from(raw[channel]) / 65535.0,
    };

    if transform.is_identity {
        // Nothing to convert: the display shows what the file holds.
        return [to_eight(raw[0], depth), to_eight(raw[1], depth), to_eight(raw[2], depth)];
    }

    let linear = [
        transform.decode_channel(normalised(0), 0),
        transform.decode_channel(normalised(1), 1),
        transform.decode_channel(normalised(2), 2),
    ];

    let mut out = [0u8; 3];
    for (row, value) in transform.matrix.iter().zip(out.iter_mut()) {
        let light = row[0] * linear[0] + row[1] * linear[1] + row[2] * linear[2];
        *value = (encode_srgb(light) * 255.0).round().clamp(0.0, 255.0) as u8;
    }
    out
}

/// Linear light to an sRGB-encoded value, the same curve as the shader's.
fn encode_srgb(value: f32) -> f32 {
    let clamped = value.clamp(0.0, 1.0);
    if clamped <= 0.003_130_8 {
        clamped * 12.92
    } else {
        1.055 * clamped.powf(1.0 / 2.4) - 0.055
    }
}

impl From<(u16, u16, u16)> for RawChannels {
    fn from(value: (u16, u16, u16)) -> Self {
        Self([value.0, value.1, value.2])
    }
}

/// A newtype only so the tuple above can be written as an array.
struct RawChannels([u16; 3]);

impl From<RawChannels> for [u16; 3] {
    fn from(value: RawChannels) -> Self {
        value.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(width: u32, height: u32, pixels: Vec<u8>) -> DecodedImage {
        DecodedImage {
            width,
            height,
            pixels,
            depth: Depth::Eight,
        }
    }

    /// A 2x2 image with a different colour in each corner, so a mix-up of the
    /// axes cannot pass.
    fn corners() -> DecodedImage {
        image(
            2,
            2,
            vec![
                10, 0, 0, 255, // (0,0)
                0, 20, 0, 255, // (1,0)
                0, 0, 30, 255, // (0,1)
                40, 40, 40, 255, // (1,1)
            ],
        )
    }

    #[test]
    fn a_pixel_is_read_where_it_was_asked_for() {
        let image = corners();
        let transform = ColorTransform::identity();

        let read_at = |x, y| read(&image, Orientation::Normal, &transform, (x, y)).expect("inside the image").file;
        assert_eq!(read_at(0, 0), [10, 0, 0]);
        assert_eq!(read_at(1, 0), [0, 20, 0]);
        assert_eq!(read_at(0, 1), [0, 0, 30]);
        assert_eq!(read_at(1, 1), [40, 40, 40]);
    }

    #[test]
    fn a_pixel_outside_the_picture_is_not_read() {
        let image = corners();
        let transform = ColorTransform::identity();
        assert_eq!(read(&image, Orientation::Normal, &transform, (2, 0)), None);
        assert_eq!(read(&image, Orientation::Normal, &transform, (0, 2)), None);
    }

    /// Where the shader draws a given stored pixel, derived from the same
    /// matrix `gpu.rs` hands it.
    ///
    /// This is the independent answer `to_stored` is checked against. Deriving
    /// it here rather than reusing the table under test is the whole point: a
    /// table checked against itself agrees with itself, which is exactly how a
    /// swapped pair of quarter turns survived the first version of these tests
    /// — it is still a one-to-one mapping, so counting pixels proved nothing.
    fn shown_at(stored: (u32, u32), image: (u32, u32), orientation: Orientation) -> (u32, u32) {
        // The matrix maps a corner of the quad to the texel it shows, in
        // centred coordinates where the axes run -1..1. Working in half-pixel
        // centres keeps the arithmetic exact for any size.
        let matrix = orientation.matrix();
        let shown = if orientation.swaps_axes() { (image.1, image.0) } else { image };

        // The stored pixel's centre, in -1..1 with y down.
        let sx = (stored.0 as f32 + 0.5) / image.0 as f32 * 2.0 - 1.0;
        let sy = (stored.1 as f32 + 0.5) / image.1 as f32 * 2.0 - 1.0;

        // The shader maps a shown position through the matrix to a stored one,
        // so going the other way is the transpose — which for these matrices,
        // all of them orthogonal, is the inverse.
        let dx = f32::from(matrix[0][0]) * sx + f32::from(matrix[1][0]) * sy;
        let dy = f32::from(matrix[0][1]) * sx + f32::from(matrix[1][1]) * sy;

        let x = ((dx + 1.0) / 2.0 * shown.0 as f32).floor().clamp(0.0, (shown.0 - 1) as f32) as u32;
        let y = ((dy + 1.0) / 2.0 * shown.1 as f32).floor().clamp(0.0, (shown.1 - 1) as f32) as u32;
        (x, y)
    }

    /// The eyedropper's inverse agrees with what the shader draws, pixel by
    /// pixel, for every orientation.
    ///
    /// Found by mutation: the first version of this test only checked that
    /// every stored pixel was shown exactly once, and both a swapped pair of
    /// quarter turns and an unmirrored flip passed it — each is still a
    /// bijection. The failure ADR 0018 records, met from the other side: a
    /// check that cannot tell a rotation from its mirror is not a check.
    #[test]
    fn the_inverse_agrees_with_what_the_shader_draws() {
        const EVERY: [Orientation; 8] = [
            Orientation::Normal,
            Orientation::FlipHorizontal,
            Orientation::Rotate180,
            Orientation::FlipVertical,
            Orientation::Transpose,
            Orientation::Rotate90,
            Orientation::Transverse,
            Orientation::Rotate270,
        ];

        // Not square, so an orientation that exchanges the axes cannot hide.
        let image = (3u32, 2u32);
        for orientation in EVERY {
            let shown = if orientation.swaps_axes() { (image.1, image.0) } else { image };
            for y in 0..image.1 {
                for x in 0..image.0 {
                    let place = shown_at((x, y), image, orientation);
                    let back = to_stored(place, shown, orientation);
                    assert_eq!(
                        back,
                        (x, y),
                        "{orientation:?}: the shader draws stored ({x}, {y}) at {place:?}, but the eyedropper reads {back:?} there",
                    );
                }
            }
        }
    }

    /// The eyedropper points at the picture as shown, so a turned image has to
    /// map back to where the pixel actually lives. Every orientation, checked
    /// by round trip: what is drawn at a place must read back as the pixel
    /// drawn there.
    #[test]
    fn every_orientation_maps_back_to_the_pixel_that_is_shown() {
        const EVERY: [Orientation; 8] = [
            Orientation::Normal,
            Orientation::FlipHorizontal,
            Orientation::Rotate180,
            Orientation::FlipVertical,
            Orientation::Transpose,
            Orientation::Rotate90,
            Orientation::Transverse,
            Orientation::Rotate270,
        ];

        // A 3x2 image, so an orientation that exchanges the axes cannot be
        // hidden by a square.
        let mut pixels = Vec::new();
        for y in 0..2u8 {
            for x in 0..3u8 {
                pixels.extend_from_slice(&[x * 10 + 1, y * 10 + 1, 0, 255]);
            }
        }
        let image = image(3, 2, pixels);
        let transform = ColorTransform::identity();

        for orientation in EVERY {
            let shown = shown_size(&image, orientation);
            let mut seen = Vec::new();
            for y in 0..shown.1 {
                for x in 0..shown.0 {
                    let reading = read(&image, orientation, &transform, (x, y))
                        .unwrap_or_else(|| panic!("{orientation:?} could not read ({x}, {y}) of a {shown:?} picture"));
                    seen.push(reading.file);
                }
            }

            // Every stored pixel is shown exactly once: an orientation that
            // maps two places to one pixel, or misses one, fails here.
            seen.sort();
            let before = seen.len();
            seen.dedup();
            assert_eq!(seen.len(), before, "{orientation:?} shows the same pixel twice, so another one is unreachable",);
            assert_eq!(before, 6, "{orientation:?} did not cover the whole picture");
        }
    }

    /// A quarter turn exchanges the axes, so the shown size does too — and a
    /// position valid for the turned picture would be outside the stored one.
    #[test]
    fn a_quarter_turn_exchanges_which_positions_are_inside() {
        let image = image(3, 2, vec![0; 3 * 2 * 4]);
        let transform = ColorTransform::identity();

        // Upright: 3 wide, 2 tall.
        assert!(read(&image, Orientation::Normal, &transform, (2, 1)).is_some());
        assert!(read(&image, Orientation::Normal, &transform, (0, 2)).is_none());

        // Turned: 2 wide, 3 tall.
        assert!(read(&image, Orientation::Rotate90, &transform, (1, 2)).is_some());
        assert!(read(&image, Orientation::Rotate90, &transform, (2, 0)).is_none());
    }

    #[test]
    fn a_sixteen_bit_pixel_is_reported_at_both_depths() {
        let sample = 0x8040u16.to_ne_bytes();
        let opaque = 0xffffu16.to_ne_bytes();
        let pixels: Vec<u8> = [sample, sample, sample, opaque].concat();
        let image = DecodedImage {
            width: 1,
            height: 1,
            pixels,
            depth: Depth::Sixteen,
        };

        let reading = read(&image, Orientation::Normal, &ColorTransform::identity(), (0, 0)).expect("inside the image");
        assert_eq!(reading.raw, [0x8040, 0x8040, 0x8040], "the full-depth value was lost");
        assert_eq!(reading.file, [0x80, 0x80, 0x80], "the eight-bit value is not the top byte");
        assert_eq!(reading.alpha, 255);
    }

    #[test]
    fn the_hex_is_the_files_colour() {
        let image = image(1, 1, vec![0xAB, 0xCD, 0xEF, 255]);
        let reading = read(&image, Orientation::Normal, &ColorTransform::identity(), (0, 0)).expect("inside the image");
        assert_eq!(reading.hex(), "#ABCDEF");
    }

    /// An untagged image needs no conversion, so the two readings agree and
    /// the panel has nothing extra to say.
    #[test]
    fn an_unconverted_pixel_reads_the_same_both_ways() {
        let image = image(1, 1, vec![120, 130, 140, 255]);
        let reading = read(&image, Orientation::Normal, &ColorTransform::identity(), (0, 0)).expect("inside the image");

        assert_eq!(reading.file, reading.display);
        assert!(!reading.converted(), "an identity transform was reported as a conversion");
    }

    /// A wide-gamut image on an sRGB display is exactly the case the second
    /// reading exists for: the same numbers mean a different colour, and a
    /// saturated one moves visibly.
    #[test]
    fn a_converted_pixel_reads_differently_on_the_display() {
        let transform = ColorTransform::new(&moxcms::ColorProfile::new_display_p3(), &moxcms::ColorProfile::new_srgb());
        // A red inside sRGB's gamut. P3's *fullest* red is outside it and
        // clips straight back to 255, which reads as no conversion at all —
        // measured, and the reason this fixture is not the obvious [255,0,0].
        let image = image(1, 1, vec![200, 60, 60, 255]);
        let reading = read(&image, Orientation::Normal, &transform, (0, 0)).expect("inside the image");

        assert_eq!(reading.file, [200, 60, 60], "the file's own value was altered");
        assert!(
            reading.converted(),
            "a P3 red on an sRGB display read as unconverted: {:?} vs {:?}",
            reading.file,
            reading.display,
        );
        // A wider red in a narrower space is a stronger number: P3 says "this
        // saturated", and sRGB has to push its own primary further to match.
        assert!(
            reading.display[0] > reading.file[0],
            "the conversion did not push the red further: {:?} vs {:?}",
            reading.file,
            reading.display,
        );
    }

    /// A neutral grey is neutral in every profile, so a conversion must leave
    /// it alone — this is what catches a matrix applied the wrong way round,
    /// which a saturated colour alone would not show.
    #[test]
    fn a_conversion_leaves_a_neutral_grey_neutral() {
        let transform = ColorTransform::new(&moxcms::ColorProfile::new_display_p3(), &moxcms::ColorProfile::new_srgb());
        let image = image(1, 1, vec![128, 128, 128, 255]);
        let reading = read(&image, Orientation::Normal, &transform, (0, 0)).expect("inside the image");

        for channel in 0..3 {
            assert!(
                reading.display[channel].abs_diff(128) <= 3,
                "a neutral grey came out as {:?}, which is not neutral",
                reading.display,
            );
        }
    }

    #[test]
    fn a_reading_one_step_apart_is_not_called_a_conversion() {
        let reading = Reading {
            at: (0, 0),
            file: [100, 100, 100],
            display: [101, 99, 100],
            alpha: 255,
            raw: [100, 100, 100],
            depth: Depth::Eight,
        };
        assert!(!reading.converted(), "a rounding step was reported as a conversion");
    }

    #[test]
    fn a_truncated_image_does_not_panic() {
        // The header claims more pixels than the buffer holds.
        let image = image(4, 4, vec![0; 8]);
        assert_eq!(read(&image, Orientation::Normal, &ColorTransform::identity(), (3, 3)), None);
    }
}
